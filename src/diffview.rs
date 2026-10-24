//! The diff viewer: one changed file at a time from a list (the Git tab's
//! files, a folder's changes, or a single file), unified or split, with a
//! cursor, hunk / file navigation, folds that expand, word highlights,
//! syntax colour, viewed marks and a choice of comparison.

use crate::diff::{self, Cmp, DiffFile, Opts, Row, Syntax};
use crate::entry::{Branch, Git};
use crate::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug)]
pub struct FileItem {
    pub path: PathBuf,
    /// Path relative to the repository root.
    pub rel: String,
    pub git: Option<Git>,
}

impl FileItem {
    fn untracked(&self) -> bool {
        self.git.is_some_and(|g| g.letter == '?')
    }
}

/// `split` config: `auto` splits when the body is this wide.
pub const AUTO_SPLIT: u16 = 160;
/// The file pane appears from this width (with more than one file).
pub const PANE_MIN: u16 = 120;
const PANE_W: u16 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Auto,
    Unified,
    Split,
}

impl Layout {
    pub fn parse(s: &str) -> Option<Layout> {
        Some(match s {
            "auto" => Layout::Auto,
            "unified" => Layout::Unified,
            "split" => Layout::Split,
            _ => return None,
        })
    }
}

struct Loaded {
    file: Option<Rc<DiffFile>>,
    error: Option<String>,
    total_new: Option<usize>,
}

/// What a key asks the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    None,
    Back,
    /// Plain file view (`d`).
    FileView,
    Flash(String),
    /// Open the current file at this line in `$EDITOR` (popup) or elsewhere.
    Edit(PathBuf, Option<usize>, Option<crate::config::OpenTarget>),
    /// Send `path:line` to the origin pane.
    Send(PathBuf, Option<usize>),
    /// Send this text (a hunk) to the origin pane.
    SendText(String),
    Copy(String),
    SystemOpen(PathBuf),
    /// Start the `/` prompt.
    Search,
}

pub struct DiffView {
    pub root: PathBuf,
    pub files: Vec<FileItem>,
    pub cur: usize,
    pub cmp: Cmp,
    pub ignore_ws: bool,
    pub layout: Layout,
    /// `s` flipped the layout for this session.
    pub split_override: Option<bool>,
    context: HashMap<PathBuf, usize>,
    pub cursor: usize,
    pub top: usize,
    pub hscroll: usize,
    pub height: usize,
    pub width: u16,
    pending: Option<char>,
    pub branch: Option<Branch>,
    pub numstat: HashMap<String, (usize, usize)>,
    cache: HashMap<(PathBuf, Opts), Loaded>,
    syntax: HashMap<(PathBuf, Cmp), Option<Rc<Syntax>>>,
    pub search: String,
    pub hits: Vec<usize>,
}

impl DiffView {
    pub fn new(root: PathBuf, files: Vec<FileItem>, cur: usize, layout: Layout) -> DiffView {
        let numstat = diff::numstat(&root, Cmp::WorkTree);
        let branch = crate::entry::git_status_full(&root).1;
        DiffView {
            root,
            cur: cur.min(files.len().saturating_sub(1)),
            files,
            cmp: Cmp::WorkTree,
            ignore_ws: false,
            layout,
            split_override: None,
            context: HashMap::new(),
            cursor: 0,
            top: 0,
            hscroll: 0,
            height: 20,
            width: 80,
            pending: None,
            branch,
            numstat,
            cache: HashMap::new(),
            syntax: HashMap::new(),
            search: String::new(),
            hits: vec![],
        }
    }

    pub fn current(&self) -> Option<&FileItem> {
        self.files.get(self.cur)
    }

    fn opts(&self) -> Opts {
        let ctx = self.current().and_then(|f| self.context.get(&f.path)).copied().unwrap_or(3);
        Opts { cmp: self.cmp, ignore_ws: self.ignore_ws, context: ctx }
    }

    pub fn split(&self) -> bool {
        self.split_override.unwrap_or(match self.layout {
            Layout::Split => true,
            Layout::Unified => false,
            Layout::Auto => self.width >= AUTO_SPLIT,
        })
    }

    /// Drop parsed diffs (after the files changed, e.g. back from the editor).
    pub fn invalidate(&mut self) {
        self.cache.clear();
        self.syntax.clear();
        self.numstat = diff::numstat(&self.root, self.cmp);
    }

    fn load(&mut self) -> &Loaded {
        let o = self.opts();
        let Some(item) = self.current().cloned() else {
            let key = (PathBuf::new(), o);
            return self.cache.entry(key).or_insert(Loaded { file: None, error: Some("no changed files".into()), total_new: None });
        };
        let key = (item.path.clone(), o);
        self.cache.entry(key).or_insert_with(|| {
            let (file, error) = match diff::load(&item.path, item.untracked(), o) {
                Ok(fs) => (fs.into_iter().next().map(Rc::new), None),
                Err(e) => (None, Some(e)),
            };
            // the new side's length, for the fold after the last hunk
            let total_new = match o.cmp {
                Cmp::Staged => None,
                _ => std::fs::metadata(&item.path).ok().filter(|m| m.is_file() && m.len() < 8 << 20).and_then(|_| std::fs::read(&item.path).ok()).map(|b| b.split(|&c| c == b'\n').count().saturating_sub(usize::from(b.ends_with(b"\n")))),
            };
            Loaded { file, error, total_new }
        })
    }

    pub fn file(&mut self) -> Option<Rc<DiffFile>> {
        self.load().file.clone()
    }

    /// Cheap identity of a file's current change (its numstat line, size
    /// and mtime): a viewed mark is dropped once the file changes again,
    /// without running a diff for every file in the list.
    pub fn fingerprint(&self, i: usize) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        if let Some(it) = self.files.get(i) {
            it.rel.hash(&mut h);
            self.numstat.get(&it.rel).hash(&mut h);
            let m = std::fs::symlink_metadata(&it.path).ok();
            m.as_ref().map(|m| m.len()).hash(&mut h);
            m.and_then(|m| m.modified().ok()).hash(&mut h);
        }
        h.finish()
    }

    pub fn rows(&mut self) -> Vec<Row> {
        let split = self.split();
        let l = self.load();
        match &l.file {
            Some(f) => diff::rows(f, split, l.total_new),
            None => vec![Row::Note],
        }
    }

    /// bat colours for both sides of the current file (cached; None when
    /// bat is missing or the file is big).
    fn syntax(&mut self) -> Option<Rc<Syntax>> {
        let item = self.current()?.clone();
        let key = (item.path.clone(), self.cmp);
        if let Some(s) = self.syntax.get(&key) {
            return s.clone();
        }
        crate::preview::bat_bin()?;
        let file = self.file();
        let name = item.rel.clone();
        let old_rel = file.as_ref().and_then(|f| f.old_path.clone()).unwrap_or_else(|| item.rel.clone());
        let text = |b: Option<Vec<u8>>| b.map(|b| String::from_utf8_lossy(&b).into_owned());
        let base = crate::git::base(&self.root);
        let old = match self.cmp {
            _ if item.untracked() => None,
            Cmp::Unstaged => text(crate::git::show(&self.root, &format!(":{old_rel}"))),
            _ => text(crate::git::show(&self.root, &format!("{base}:{old_rel}"))),
        };
        let new = match self.cmp {
            Cmp::Staged => text(crate::git::show(&self.root, &format!(":{}", item.rel))),
            _ => text(std::fs::read(&item.path).ok()),
        };
        let s = Syntax {
            old: old.and_then(|t| diff::syntax_of(&t, &name)).unwrap_or_default(),
            new: new.and_then(|t| diff::syntax_of(&t, &name)).unwrap_or_default(),
        };
        let s = (!s.old.is_empty() || !s.new.is_empty()).then(|| Rc::new(s));
        self.syntax.insert(key, s.clone());
        s
    }

    fn go_file(&mut self, i: usize) {
        if i < self.files.len() && i != self.cur {
            self.cur = i;
            self.cursor = 0;
            self.top = 0;
            self.hscroll = 0;
            self.hits.clear();
        }
    }

    fn clamp(&mut self, n: usize) {
        self.cursor = self.cursor.min(n.saturating_sub(1));
        let h = self.height.max(1);
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + h {
            self.top = self.cursor + 1 - h;
        }
        self.top = self.top.min(n.saturating_sub(h.min(n)));
    }

    /// Re-run with more context, keeping the cursor on the same line.
    fn expand(&mut self, all: bool) {
        let Some(item) = self.current().cloned() else { return };
        let (file, rows) = (self.file(), self.rows());
        let at = file.as_ref().and_then(|f| rows.get(self.cursor).and_then(|r| diff::row_line(f, *r)));
        let ctx = self.context.entry(item.path).or_insert(3);
        *ctx = if all { 1_000_000 } else { *ctx + 20 };
        let (file, rows) = (self.file(), self.rows());
        if let (Some(f), Some(at)) = (file, at) {
            if let Some(i) = rows.iter().position(|r| matches!(r, Row::Line { .. } | Row::Pair { .. }) && diff::row_line(&f, *r) == Some(at)) {
                self.cursor = i;
                self.top = i.saturating_sub(self.height / 3);
            }
        }
    }

    /// Rows matching the search (smart case).
    pub fn find(&mut self) {
        let Some(f) = self.file() else {
            self.hits.clear();
            return;
        };
        let smart = self.search.chars().any(|c| c.is_uppercase());
        let pat = if smart { self.search.clone() } else { self.search.to_lowercase() };
        self.hits = self
            .rows()
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                let t = diff::row_text(&f, **r);
                !pat.is_empty() && if smart { t.contains(&pat) } else { t.to_lowercase().contains(&pat) }
            })
            .map(|(i, _)| i)
            .collect();
    }

    pub fn jump_hit(&mut self, fwd: bool) -> Out {
        if self.search.is_empty() {
            return Out::Flash("no search yet — / to search".into());
        }
        let hit = if fwd { self.hits.iter().find(|&&h| h > self.cursor).or(self.hits.first()) } else { self.hits.iter().rev().find(|&&h| h < self.cursor).or(self.hits.last()) };
        match hit.copied() {
            Some(h) => {
                self.cursor = h;
                self.top = h.saturating_sub(self.height / 3);
                let i = self.hits.iter().position(|&x| x == h).unwrap_or(0);
                Out::Flash(format!("/{}  [{}/{}]", self.search, i + 1, self.hits.len()))
            }
            None => Out::Flash(format!("pattern not found: {}", self.search)),
        }
    }

    /// The hunk under the cursor as `diff` text (for sending).
    fn hunk_text(&mut self) -> Option<String> {
        let f = self.file()?;
        let rows = self.rows();
        let hi = match rows.get(self.cursor)? {
            Row::Hunk(h) | Row::Line { h, .. } | Row::Pair { h, .. } => *h,
            Row::Fold { h, .. } => (*h).min(f.hunks.len().checked_sub(1)?),
            Row::Note => return None,
        };
        let h = f.hunks.get(hi)?;
        let mut s = format!("```diff\n--- a/{0}\n+++ b/{0}\n@@ -{1},{2} +{3},{4} @@ {5}\n", f.path, h.old_start, h.old_len, h.new_start, h.new_len, h.header);
        for l in &h.lines {
            let sign = match l.kind {
                diff::Kind::Add => '+',
                diff::Kind::Del => '-',
                diff::Kind::Ctx => ' ',
            };
            s.push(sign);
            s.push_str(&l.text);
            s.push('\n');
        }
        s.push_str("```");
        Some(s)
    }

    pub fn key(&mut self, k: KeyEvent, viewed: &mut HashSet<(PathBuf, u64)>) -> Out {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let rows = self.rows();
        let n = rows.len();
        let h = self.height.max(1);
        let file = self.file();
        let path = self.current().map(|f| f.path.clone());
        let line = file.as_ref().and_then(|f| rows.get(self.cursor).and_then(|r| diff::row_line(f, *r)));
        if let Some(p) = self.pending.take() {
            if let KeyCode::Char('c' | 'g') = k.code {
                let fwd = p == ']';
                let hunks: Vec<usize> = rows.iter().enumerate().filter(|(_, r)| matches!(r, Row::Hunk(_))).map(|(i, _)| i).collect();
                let hit = if fwd { hunks.iter().find(|&&i| i > self.cursor) } else { hunks.iter().rev().find(|&&i| i < self.cursor) };
                match hit {
                    Some(&i) => {
                        self.cursor = i;
                        self.top = i.saturating_sub(2);
                    }
                    None => return Out::Flash(if fwd { "last hunk — ⇥ next file".into() } else { "first hunk".into() }),
                }
                return Out::None;
            }
        }
        let out = match (k.code, ctrl) {
            (KeyCode::Char(']' | '['), false) => {
                self.pending = if let KeyCode::Char(c) = k.code { Some(c) } else { None };
                Out::None
            }
            (KeyCode::Char('j') | KeyCode::Down, false) => {
                self.cursor += 1;
                Out::None
            }
            (KeyCode::Char('k') | KeyCode::Up, false) => {
                self.cursor = self.cursor.saturating_sub(1);
                Out::None
            }
            (KeyCode::Char('d'), true) => {
                self.cursor += h / 2;
                Out::None
            }
            (KeyCode::Char('u'), true) => {
                self.cursor = self.cursor.saturating_sub(h / 2);
                Out::None
            }
            (KeyCode::Char(' ') | KeyCode::PageDown, false) | (KeyCode::Char('f'), true) => {
                self.cursor += h.saturating_sub(1);
                self.top += h.saturating_sub(1);
                Out::None
            }
            (KeyCode::Char('b') | KeyCode::PageUp, false) | (KeyCode::Char('b'), true) => {
                self.cursor = self.cursor.saturating_sub(h.saturating_sub(1));
                self.top = self.top.saturating_sub(h.saturating_sub(1));
                Out::None
            }
            (KeyCode::Char('g') | KeyCode::Home, false) => {
                self.cursor = 0;
                Out::None
            }
            (KeyCode::Char('G') | KeyCode::End, false) => {
                self.cursor = n.saturating_sub(1);
                Out::None
            }
            (KeyCode::Tab, _) => {
                if self.files.len() < 2 {
                    Out::Flash("only one file in this diff".into())
                } else {
                    self.go_file((self.cur + 1) % self.files.len());
                    Out::None
                }
            }
            (KeyCode::BackTab, _) => {
                if self.files.len() > 1 {
                    self.go_file((self.cur + self.files.len() - 1) % self.files.len());
                }
                Out::None
            }
            (KeyCode::Char('s'), false) => {
                let s = !self.split();
                self.split_override = Some(s);
                self.cursor = 0;
                self.top = 0;
                Out::Flash(if s { "split view" } else { "unified view" }.into())
            }
            (KeyCode::Char('w'), false) => {
                self.ignore_ws = !self.ignore_ws;
                self.cursor = 0;
                self.top = 0;
                Out::Flash(if self.ignore_ws { "ignoring whitespace (-w)" } else { "showing whitespace changes" }.into())
            }
            (KeyCode::Char('z'), false) => {
                self.expand(false);
                Out::None
            }
            (KeyCode::Char('Z'), false) => {
                self.expand(true);
                Out::Flash("whole file".into())
            }
            (KeyCode::Char('v'), false) => {
                let (Some(p), hash) = (path.clone(), self.fingerprint(self.cur)) else { return Out::None };
                let key = (p, hash);
                if !viewed.remove(&key) {
                    viewed.insert(key);
                    // on to the next file not viewed yet
                    let next = (1..self.files.len()).map(|d| (self.cur + d) % self.files.len()).find(|&i| !self.is_viewed(i, viewed));
                    if let Some(i) = next {
                        self.go_file(i);
                    }
                }
                Out::None
            }
            (KeyCode::Char('c'), false) => {
                self.cmp = self.cmp.next();
                self.cursor = 0;
                self.top = 0;
                self.numstat = diff::numstat(&self.root, self.cmp);
                Out::Flash(format!("comparing: {}", self.cmp.label()))
            }
            (KeyCode::Char('l'), true) => {
                self.invalidate();
                Out::Flash("reloaded".into())
            }
            (KeyCode::Char('h') | KeyCode::Left, false) => {
                self.hscroll = self.hscroll.saturating_sub(8);
                Out::None
            }
            (KeyCode::Char('l') | KeyCode::Right, false) => {
                self.hscroll += 8;
                Out::None
            }
            (KeyCode::Enter | KeyCode::Char('e'), false) => match path {
                Some(p) if self.cmp != Cmp::Staged || line.is_some() => Out::Edit(p, line, None),
                _ => Out::None,
            },
            (KeyCode::Char('v'), true) => path.map(|p| Out::Edit(p, line, Some(crate::config::OpenTarget::SplitRight))).unwrap_or(Out::None),
            (KeyCode::Char('x'), true) => path.map(|p| Out::Edit(p, line, Some(crate::config::OpenTarget::SplitDown))).unwrap_or(Out::None),
            (KeyCode::Char('t'), true) => path.map(|p| Out::Edit(p, line, Some(crate::config::OpenTarget::Tab))).unwrap_or(Out::None),
            (KeyCode::Char('p'), false) => path.map(|p| Out::Send(p, line)).unwrap_or(Out::None),
            (KeyCode::Char('P'), false) => match self.hunk_text() {
                Some(t) => Out::SendText(t),
                None => Out::Flash("no hunk here".into()),
            },
            (KeyCode::Char('y'), false) => path.map(|p| Out::Copy(p.display().to_string())).unwrap_or(Out::None),
            (KeyCode::Char('o'), false) => path.map(Out::SystemOpen).unwrap_or(Out::None),
            (KeyCode::Char('d'), false) => Out::FileView,
            (KeyCode::Char('/'), false) => Out::Search,
            (KeyCode::Char('n'), false) => self.jump_hit(true),
            (KeyCode::Char('N'), false) => self.jump_hit(false),
            (KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('-') | KeyCode::Backspace, false) => Out::Back,
            _ => Out::None,
        };
        let n = self.rows().len();
        self.clamp(n);
        out
    }

    fn is_viewed(&self, i: usize, viewed: &HashSet<(PathBuf, u64)>) -> bool {
        self.files.get(i).is_some_and(|f| viewed.contains(&(f.path.clone(), self.fingerprint(i))))
    }

    /// (added, removed) for a file: numstat, else the loaded diff.
    fn stat_of(&mut self, i: usize) -> Option<(usize, usize)> {
        let rel = self.files.get(i)?.rel.clone();
        if let Some(s) = self.numstat.get(&rel) {
            return Some(*s);
        }
        // untracked files are not in numstat: all their lines are new
        if self.files[i].untracked() && self.cmp != Cmp::Staged {
            let n = std::fs::metadata(&self.files[i].path).ok().filter(|m| m.is_file() && m.len() < 8 << 20).and_then(|_| std::fs::read(&self.files[i].path).ok()).map(|b| b.iter().filter(|&&c| c == b'\n').count() + usize::from(!b.is_empty() && !b.ends_with(b"\n")));
            if let Some(n) = n {
                self.numstat.insert(rel, (n, 0));
                return Some((n, 0));
            }
        }
        if i == self.cur {
            return self.file().map(|f| f.stat());
        }
        None
    }
}

/// Draw the diff view into `area` (top bar, optional file pane, body).
pub fn draw(f: &mut Frame, th: &Theme, dv: &mut DiffView, viewed: &HashSet<(PathBuf, u64)>, area: Rect) -> Rect {
    let [bar, rest] = ratatui::layout::Layout::vertical([ratatui::layout::Constraint::Length(1), ratatui::layout::Constraint::Min(1)]).areas(area);
    let pane = dv.files.len() > 1 && area.width >= PANE_MIN;
    let (pane_area, body) = if pane {
        let [p, b] = ratatui::layout::Layout::horizontal([ratatui::layout::Constraint::Length(PANE_W), ratatui::layout::Constraint::Min(1)]).areas(rest);
        (Some(p), b)
    } else {
        (None, rest)
    };
    dv.width = body.width;
    let viewed_n = (0..dv.files.len()).filter(|&i| dv.is_viewed(i, viewed)).count();

    // top bar: branch, totals, comparison, viewed
    let (mut ta, mut td) = (0, 0);
    for i in 0..dv.files.len() {
        if let Some((a, d)) = dv.stat_of(i) {
            ta += a;
            td += d;
        }
    }
    let mut sp = vec![Span::raw(" ")];
    if let Some(b) = &dv.branch {
        sp.push(Span::styled(format!("\u{e725} {}  ", b.badge()), Style::default().fg(th.accent).add_modifier(Modifier::BOLD)));
    }
    let nf = dv.files.len();
    sp.push(Span::styled(format!("{nf} file{} changed ", if nf == 1 { "" } else { "s" }), Style::default().fg(th.text)));
    sp.push(Span::styled(format!("+{ta} "), Style::default().fg(th.green)));
    sp.push(Span::styled(format!("−{td}  "), Style::default().fg(th.red)));
    sp.extend(diff::stat_bar(th, ta, td));
    let right = format!(
        "{}{}{}   viewed {viewed_n}/{nf} ",
        dv.cmp.label(),
        if dv.ignore_ws { " · -w" } else { "" },
        if dv.split() { " · split" } else { "" }
    );
    let used: usize = sp.iter().map(|s| s.content.width()).sum();
    sp.push(Span::raw(" ".repeat((bar.width as usize).saturating_sub(used + right.width()))));
    sp.push(Span::styled(right, Style::default().fg(th.dim)));
    f.render_widget(Paragraph::new(Line::from(sp)).style(Style::default().bg(th.surface)), bar);

    if let Some(pa) = pane_area {
        draw_pane(f, th, dv, viewed, pa);
    }

    // body: file header, split column titles, rows
    let file = dv.file();
    let err = dv.load().error.clone();
    let mut lines = Vec::new();
    let is_viewed = dv.is_viewed(dv.cur, viewed);
    let head = match &file {
        Some(fl) => {
            let mut l = diff::file_header(th, fl);
            if is_viewed {
                l.spans.push(Span::styled("  ✓ viewed", Style::default().fg(th.green)));
            }
            l
        }
        None => Line::from(vec![Span::styled(format!(" {}", dv.current().map(|c| c.rel.clone()).unwrap_or_default()), Style::default().fg(th.text).add_modifier(Modifier::BOLD))]),
    };
    lines.push(head);
    let split = dv.split();
    if split {
        let (l, r) = dv.cmp.sides();
        let half = (body.width as usize).saturating_sub(2) / 2;
        lines.push(Line::from(vec![
            Span::styled(format!(" {l:<half$}"), Style::default().fg(th.subtext).add_modifier(Modifier::BOLD)),
            Span::styled("│", Style::default().fg(th.dim)),
            Span::styled(format!(" {r}"), Style::default().fg(th.subtext).add_modifier(Modifier::BOLD)),
        ]));
    }
    let fixed = lines.len();
    let h = (body.height as usize).saturating_sub(fixed);
    dv.height = h;
    let rows = dv.rows();
    dv.clamp(rows.len());
    let syn = dv.syntax();
    let pal = diff::Palette::new(th);
    let width = (body.width as usize).saturating_sub(1);
    match (&file, err) {
        (None, Some(e)) => lines.push(Line::styled(format!("  {e}"), Style::default().fg(th.red))),
        (None, None) => lines.push(Line::styled("  no changes", Style::default().fg(th.dim).add_modifier(Modifier::ITALIC))),
        (Some(fl), _) => {
            let nw = diff::num_width(fl);
            for (i, r) in rows.iter().enumerate().skip(dv.top).take(h) {
                let mut l = if split { diff::split_row(th, &pal, fl, *r, width, dv.hscroll, syn.as_deref(), nw) } else { diff::unified_row(th, &pal, fl, *r, width, dv.hscroll, syn.as_deref(), nw) };
                let hit = dv.hits.binary_search(&i).is_ok();
                let mark = if i == dv.cursor { ("▌", th.accent) } else if hit { ("▌", th.peach) } else { (" ", th.dim) };
                l.spans.insert(0, Span::styled(mark.0, Style::default().fg(mark.1)));
                if i == dv.cursor {
                    l = l.style(Style::default().bg(th.selection));
                }
                lines.push(l);
            }
        }
    }
    f.render_widget(Paragraph::new(lines), body);
    body
}

/// Changed files as a tree: folders, then status letter, name, +a −d, ✓.
fn draw_pane(f: &mut Frame, th: &Theme, dv: &mut DiffView, viewed: &HashSet<(PathBuf, u64)>, area: Rect) {
    let w = area.width.saturating_sub(1) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut cur_line = 0;
    let mut last_dir: Option<String> = None;
    for i in 0..dv.files.len() {
        let rel = dv.files[i].rel.clone();
        let (dir, name) = match rel.rfind('/') {
            Some(k) => (rel[..=k].to_string(), rel[k + 1..].to_string()),
            None => (String::new(), rel.clone()),
        };
        if !dir.is_empty() && last_dir.as_deref() != Some(dir.as_str()) {
            let d: String = dir.chars().take(w.saturating_sub(2)).collect();
            lines.push(Line::styled(format!(" {d}"), Style::default().fg(th.subtext)));
        }
        last_dir = Some(dir.clone());
        let g = dv.files[i].git;
        let letter = g.map(|g| g.letter).unwrap_or('M');
        let gcol = g.map(|g| crate::ui::git_color(th, g)).unwrap_or(th.yellow);
        let stat = dv.stat_of(i).map(|(a, d)| format!("+{a} −{d}")).unwrap_or_default();
        let tick = if dv.is_viewed(i, viewed) { "✓" } else { " " };
        let indent = if dir.is_empty() { 1 } else { 3 };
        let room = w.saturating_sub(indent + 2 + stat.width() + 3);
        let mut shown: String = name.chars().take(room).collect();
        if shown.chars().count() < name.chars().count() && room > 1 {
            shown = name.chars().take(room - 1).collect::<String>() + "…";
        }
        let pad = room.saturating_sub(shown.width()) + 1;
        let current = i == dv.cur;
        if current {
            cur_line = lines.len();
        }
        let name_st = if current { Style::default().fg(th.text).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.subtext) };
        let mut l = Line::from(vec![
            Span::styled(if current { "▌" } else { " " }, Style::default().fg(th.accent)),
            Span::raw(" ".repeat(indent - 1)),
            Span::styled(format!("{letter} "), Style::default().fg(gcol).add_modifier(Modifier::BOLD)),
            Span::styled(shown, name_st),
            Span::raw(" ".repeat(pad)),
            Span::styled(stat, Style::default().fg(th.dim)),
            Span::styled(format!(" {tick}"), Style::default().fg(th.green)),
        ]);
        if current {
            l = l.style(Style::default().bg(th.selection));
        }
        lines.push(l);
    }
    let h = area.height as usize;
    let skip = cur_line.saturating_sub(h.saturating_sub(2));
    let blk = ratatui::widgets::Block::default().borders(ratatui::widgets::Borders::RIGHT).border_style(Style::default().fg(th.dim));
    let inner = blk.inner(area);
    f.render_widget(blk, area);
    f.render_widget(Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()), inner);
}

/// Items for a folder's (or a single file's) changes.
pub fn items_under(p: &Path) -> Result<(PathBuf, Vec<FileItem>), String> {
    let root = crate::git::guard(p)?;
    let dir = if p.is_dir() { p.to_path_buf() } else { p.parent().map(Path::to_path_buf).ok_or("no folder")? };
    let changes = crate::entry::git_changes(&dir).ok_or("not (safely) in a git repository")?;
    let mut out = Vec::new();
    for (g, rel) in changes {
        let path = dir.join(&rel);
        if !p.is_dir() && path != p {
            continue;
        }
        let rel = crate::git::rel(&root, &path).unwrap_or(rel);
        out.push(FileItem { path, rel, git: Some(g) });
    }
    Ok((root, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigate_hunks_files_and_viewed() {
        let t = crate::plan::tests::tmpdir("diffview");
        let g = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&t.0).output().unwrap();
        g(&["init", "-q"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "t"]);
        let body: String = (1..=60).map(|i| format!("line {i}\n")).collect();
        std::fs::write(t.0.join("a.txt"), &body).unwrap();
        std::fs::write(t.0.join("b.txt"), "x\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-qm", "x"]);
        std::fs::write(t.0.join("a.txt"), body.replace("line 5\n", "line five\n").replace("line 50\n", "line fifty\n")).unwrap();
        std::fs::write(t.0.join("b.txt"), "y\n").unwrap();
        let (root, items) = items_under(&t.0).unwrap();
        assert_eq!(items.len(), 2);
        let mut dv = DiffView::new(root, items, 0, Layout::Unified);
        dv.height = 10;
        let rows = dv.rows();
        assert!(matches!(rows[0], Row::Fold { .. }) || matches!(rows[0], Row::Hunk(0)));
        assert!(matches!(rows.last(), Some(Row::Fold { .. })), "fold after the last hunk: {rows:?}");
        let mut viewed = HashSet::new();
        let key = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        dv.key(key(']'), &mut viewed);
        dv.key(key('c'), &mut viewed);
        let first = dv.cursor;
        assert!(matches!(dv.rows()[first], Row::Hunk(_)));
        dv.key(key(']'), &mut viewed);
        dv.key(key('c'), &mut viewed);
        assert!(dv.cursor > first);
        let n_before = dv.rows().len();
        dv.key(key('z'), &mut viewed);
        assert!(dv.rows().len() > n_before, "z shows more context");
        assert_eq!(dv.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut viewed), Out::Edit(t.0.join("a.txt"), dv.file().and_then(|f| diff::row_line(&f, dv.rows()[dv.cursor])), None));
        dv.key(key('v'), &mut viewed);
        assert_eq!(viewed.len(), 1);
        assert_eq!(dv.cur, 1, "v moves on to the next unviewed file");
        dv.key(key('s'), &mut viewed);
        assert!(dv.split() && dv.rows().iter().any(|r| matches!(r, Row::Pair { .. })));
        // the file changes again: its ✓ goes away
        dv.go_file(0);
        assert!(dv.is_viewed(0, &viewed));
        std::fs::write(t.0.join("a.txt"), body.replace("line 6\n", "six\n")).unwrap();
        dv.invalidate();
        assert!(!dv.is_viewed(0, &viewed));
    }
}
