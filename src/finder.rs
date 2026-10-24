//! Fuzzy finder: files / directories (parallel `ignore` walk into nucleo),
//! live grep (ripgrep, falling back to grep), recently opened files, and
//! projects (see [`crate::projects`]).

use crate::projects::{self, Proj, Scope};
use crate::vim::FinderKind;
use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config as NConfig, Matcher, Nucleo};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Hit {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
}

/// A marked finder entry: grep hits keep their line, so several hits in
/// one file stay distinct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub path: PathBuf,
    pub line: Option<usize>,
}

pub struct Item {
    pub display: String,
    pub abs: PathBuf,
    pub line: Option<usize>,
    /// Char indices into `display` to highlight.
    pub indices: Vec<u32>,
    pub is_dir: bool,
    pub grep_text: Option<String>,
    /// Git tab: status (M A D R U ?, staged / unstaged).
    pub git: Option<crate::entry::Git>,
    /// Dashboard: why the item is listed (`+ ~ → ⧉ −` pending edit, `◷` recent).
    pub tag: Option<char>,
    /// Projects tab: the project behind the row.
    pub proj: Option<Proj>,
}

pub struct Finder {
    pub kind: FinderKind,
    pub root: PathBuf,
    /// Root the Git tab replaced with the repository root; restored when
    /// leaving the tab (unless `set_root` picked another one meanwhile).
    pub pre_git: Option<PathBuf>,
    pub query: String,
    pub qcur: usize,
    pub sel: usize,
    pub scroll: usize,
    pub hidden: bool,
    pub no_ignore: bool,
    pub marked: Vec<Mark>,
    max_files: usize,
    nucleo: Nucleo<String>,
    matcher: Matcher,
    walk_cancel: Arc<AtomicBool>,
    walking: Arc<AtomicBool>,
    walked: Arc<AtomicUsize>,
    pub dirty: Arc<AtomicBool>,
    grep: Grep,
    recent_file: PathBuf,
    /// Git tab: status per root-relative path; None outside a repository.
    pub git: Option<std::collections::HashMap<String, crate::entry::Git>>,
    /// Dashboard: pending edits handed in by the app (tag, path).
    pub pending: Vec<(char, PathBuf)>,
    /// Dashboard: listed string → (tag, absolute path).
    dash: std::collections::HashMap<String, (char, PathBuf)>,
    /// Projects tab: listed string → row (filled by a worker thread; the
    /// dirty counts land a little later).
    projects: Arc<Mutex<std::collections::HashMap<String, Proj>>>,
    pub proj_scope: Scope,
    pub proj_settings: projects::Settings,
    /// Where the finder was opened: marks the project you are in.
    here: PathBuf,
}

struct Grep {
    hits: Arc<Mutex<Vec<Hit>>>,
    running: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    launched: Option<String>,
    changed_at: Instant,
    truncated: Arc<AtomicBool>,
}

const GREP_CAP: usize = 5000;

/// Split a grep query into the pattern and ripgrep filters given after
/// ` -- `: `-t rust` / `-T md` (types), `-g x` or a bare `*.rs` (globs).
/// Only these become arguments, never arbitrary flags.
pub fn grep_query(q: &str) -> (String, Vec<String>) {
    let Some((pat, rest)) = q.split_once(" -- ") else { return (q.to_string(), vec![]) };
    let mut out = Vec::new();
    let mut toks = rest.split_whitespace();
    while let Some(t) = toks.next() {
        let (flag, val) = match t {
            "-t" | "-T" | "-g" => (t, toks.next().unwrap_or("")),
            _ if t.len() > 2 && (t.starts_with("-t") || t.starts_with("-T") || t.starts_with("-g")) => t.split_at(2),
            _ if t.starts_with('-') => continue,
            _ => ("-g", t),
        };
        if val.is_empty() {
            continue;
        }
        out.push(match flag {
            "-t" => format!("--type={val}"),
            "-T" => format!("--type-not={val}"),
            _ => format!("--glob={val}"),
        });
    }
    (pat.to_string(), out)
}

fn rg_available() -> bool {
    static RG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *RG.get_or_init(|| Command::new("rg").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success()))
}

fn new_nucleo(dirty: &Arc<AtomicBool>) -> Nucleo<String> {
    let d = dirty.clone();
    Nucleo::new(NConfig::DEFAULT.match_paths(), Arc::new(move || d.store(true, Ordering::Relaxed)), None, 1)
}

/// Git work-tree root of `p`, if any.
pub fn repo_root(p: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(p)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then(|| PathBuf::from(s))
}

pub fn tilde(p: &Path) -> String {
    let s = p.display().to_string();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

impl Finder {
    pub fn new(kind: FinderKind, root: PathBuf, cfg: &crate::config::Config) -> Finder {
        let dirty = Arc::new(AtomicBool::new(true));
        let here = root.clone();
        let (root, pre_git) = match kind {
            FinderKind::Git => (repo_root(&root).unwrap_or_else(|| root.clone()), Some(root)),
            _ => (root, None),
        };
        let mut f = Finder {
            kind,
            root,
            pre_git,
            query: String::new(),
            qcur: 0,
            sel: 0,
            scroll: 0,
            hidden: cfg.show_hidden,
            no_ignore: !cfg.respect_ignore,
            marked: vec![],
            max_files: cfg.max_files,
            nucleo: new_nucleo(&dirty),
            matcher: Matcher::new(NConfig::DEFAULT.match_paths()),
            walk_cancel: Arc::new(AtomicBool::new(false)),
            walking: Arc::new(AtomicBool::new(false)),
            walked: Arc::new(AtomicUsize::new(0)),
            dirty,
            grep: Grep {
                hits: Arc::new(Mutex::new(vec![])),
                running: Arc::new(AtomicBool::new(false)),
                cancel: Arc::new(AtomicBool::new(false)),
                child: Arc::new(Mutex::new(None)),
                launched: None,
                changed_at: Instant::now(),
                truncated: Arc::new(AtomicBool::new(false)),
            },
            recent_file: cfg.state_dir.join("recent.txt"),
            git: None,
            pending: vec![],
            dash: Default::default(),
            projects: Default::default(),
            proj_scope: Scope::All,
            proj_settings: cfg.projects.clone(),
            here,
        };
        f.restart();
        f
    }

    pub fn set_kind(&mut self, k: FinderKind) {
        if self.kind != k {
            if k == FinderKind::Git {
                let top = repo_root(&self.root).unwrap_or_else(|| self.root.clone());
                self.pre_git = Some(std::mem::replace(&mut self.root, top));
            } else if let Some(r) = self.pre_git.take() {
                self.root = r;
            }
            self.kind = k;
            self.sel = 0;
            self.scroll = 0;
            self.restart();
        }
    }

    pub fn set_root(&mut self, root: PathBuf) {
        self.root = root;
        self.pre_git = None;
        self.sel = 0;
        self.scroll = 0;
        self.restart();
    }

    pub fn toggle_hidden(&mut self) {
        self.hidden = !self.hidden;
        self.restart();
    }

    pub fn toggle_ignore(&mut self) {
        self.no_ignore = !self.no_ignore;
        self.restart();
    }

    pub fn walking(&self) -> bool {
        self.walking.load(Ordering::Relaxed) || self.grep.running.load(Ordering::Relaxed)
    }

    pub fn total(&self) -> usize {
        match self.kind {
            FinderKind::Grep => self.grep.hits.lock().map(|h| h.len()).unwrap_or(0),
            _ => self.nucleo.snapshot().item_count() as usize,
        }
    }

    pub fn truncated(&self) -> bool {
        match self.kind {
            FinderKind::Grep => self.grep.truncated.load(Ordering::Relaxed),
            _ => self.walked.load(Ordering::Relaxed) >= self.max_files,
        }
    }

    fn stop(&mut self) {
        self.walk_cancel.store(true, Ordering::Relaxed);
        self.grep.cancel.store(true, Ordering::Relaxed);
        if let Ok(mut c) = self.grep.child.lock() {
            if let Some(mut ch) = c.take() {
                let _ = ch.kill();
                let _ = ch.wait();
            }
        }
    }

    /// (Re)start the item source for the current kind/root/flags.
    pub fn restart(&mut self) {
        self.stop();
        self.nucleo = new_nucleo(&self.dirty);
        self.walked.store(0, Ordering::Relaxed);
        self.grep.launched = None;
        if let Ok(mut h) = self.grep.hits.lock() {
            h.clear();
        }
        self.reparse(false);
        match self.kind {
            FinderKind::Files | FinderKind::Dirs => self.start_walk(),
            FinderKind::Recent => {
                let inj = self.nucleo.injector();
                for p in read_recent(&self.recent_file) {
                    if p.exists() {
                        let s = tilde(&p);
                        inj.push(s, |s, cols| cols[0] = s.as_str().into());
                    }
                }
            }
            FinderKind::Grep => {
                self.grep.changed_at = Instant::now() - Duration::from_secs(1);
            }
            FinderKind::Git => {
                let changes = crate::entry::git_changes(&self.root);
                let inj = self.nucleo.injector();
                for (_, p) in changes.iter().flatten() {
                    inj.push(p.clone(), |s, cols| cols[0] = s.as_str().into());
                }
                self.git = changes.map(|v| v.into_iter().map(|(c, p)| (p, c)).collect());
            }
            FinderKind::Dash => self.fill_dash(),
            FinderKind::Projects => self.start_projects(),
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Projects tab: `^a` cycles all → open → dirty → recent.
    pub fn cycle_scope(&mut self) {
        self.proj_scope = self.proj_scope.next();
        self.sel = 0;
        self.scroll = 0;
        self.restart();
    }

    /// List the projects off the UI thread (herdr and the index take a
    /// moment), then check git state for the rows that show it: recent and
    /// open ones, or every project for the dirty scope (which lists only
    /// those with changes or unpushed commits).
    fn start_projects(&mut self) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.walk_cancel = cancel.clone();
        let rows = Arc::new(Mutex::new(std::collections::HashMap::new()));
        self.projects = rows.clone();
        let (walking, dirty, inj) = (self.walking.clone(), self.dirty.clone(), self.nucleo.injector());
        let (s, scope, here) = (self.proj_settings.clone(), self.proj_scope, self.here.clone());
        walking.store(true, Ordering::Relaxed);
        std::thread::spawn(move || {
            let mut list = projects::collect(&s, scope, &here);
            // listed string: name and branch (so branches are searchable), unique
            let mut keys: Vec<String> = Vec::with_capacity(list.len());
            for r in &list {
                let mut k = match &r.head {
                    Some(h) => format!("{}  {}", r.name, h.name),
                    None => r.name.clone(),
                };
                if keys.contains(&k) {
                    k = format!("{}  {}", tilde(&r.path), r.head.as_ref().map(|h| h.name.as_str()).unwrap_or(""));
                }
                keys.push(k);
            }
            let inject = |list: &[Proj], keys: &[String]| {
                let mut m = rows.lock().unwrap();
                for (r, k) in list.iter().zip(keys) {
                    m.insert(k.clone(), r.clone());
                    inj.push(k.clone(), |s, cols| cols[0] = s.as_str().into());
                }
            };
            if scope != Scope::Dirty {
                inject(&list, &keys);
                dirty.store(true, Ordering::Relaxed);
            }
            let check: Vec<usize> = (0..list.len()).filter(|&i| scope == Scope::Dirty || list[i].hopped.is_some() || list[i].ws.is_some()).collect();
            let next = AtomicUsize::new(0);
            let found = Mutex::new(Vec::new());
            std::thread::scope(|sc| {
                for _ in 0..8 {
                    sc.spawn(|| loop {
                        let n = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&i) = check.get(n) else { break };
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        if let Some(d) = projects::dirty(&list[i].path) {
                            found.lock().unwrap().push((i, d));
                        }
                    });
                }
            });
            // a newer scan owns `walking` now
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            for (i, d) in found.into_inner().unwrap() {
                list[i].dirty = Some(d);
                if let Some(r) = rows.lock().unwrap().get_mut(&keys[i]) {
                    r.dirty = Some(d);
                }
            }
            if scope == Scope::Dirty {
                let (l, k): (Vec<Proj>, Vec<String>) = list.into_iter().zip(keys).filter(|(r, _)| r.dirty.is_some_and(|(n, a)| n > 0 || a > 0)).unzip();
                inject(&l, &k);
            }
            walking.store(false, Ordering::Relaxed);
            dirty.store(true, Ordering::Relaxed);
        });
    }

    /// Dashboard: pending edits, then git changes, marked and recent files,
    /// each path once (the first reason wins).
    fn fill_dash(&mut self) {
        self.dash.clear();
        let mut seen = std::collections::HashSet::new();
        let mut rows: Vec<(String, char, PathBuf)> = Vec::new();
        let label = |p: &Path, root: &Path| match p.strip_prefix(root) {
            Ok(r) if !r.as_os_str().is_empty() => r.display().to_string(),
            _ => tilde(p),
        };
        for (tag, p) in &self.pending {
            if seen.insert(p.clone()) {
                rows.push((label(p, &self.root), *tag, p.clone()));
            }
        }
        let changes = crate::entry::work_tree(&self.root).and_then(|r| crate::entry::git_changes(&r).map(|c| (r, c)));
        let mut git = std::collections::HashMap::new();
        if let Some((repo, changes)) = changes {
            for (g, rel) in changes {
                let p = repo.join(&rel);
                if seen.insert(p.clone()) {
                    let l = label(&p, &self.root);
                    git.insert(l.clone(), g);
                    rows.push((l, g.letter, p));
                }
            }
        }
        for m in &self.marked {
            if seen.insert(m.path.clone()) {
                rows.push((label(&m.path, &self.root), '●', m.path.clone()));
            }
        }
        for p in read_recent(&self.recent_file).into_iter().take(40) {
            if p.exists() && seen.insert(p.clone()) {
                rows.push((label(&p, &self.root), '◷', p));
            }
        }
        let inj = self.nucleo.injector();
        for (l, tag, p) in rows {
            inj.push(l.clone(), |s, cols| cols[0] = s.as_str().into());
            self.dash.insert(l, (tag, p));
        }
        self.git = Some(git);
    }

    fn start_walk(&mut self) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.walk_cancel = cancel.clone();
        let walking = self.walking.clone();
        let walked = self.walked.clone();
        let inj = self.nucleo.injector();
        let root = self.root.clone();
        let dirs = self.kind == FinderKind::Dirs;
        let (hidden, no_ignore, cap) = (self.hidden, self.no_ignore, self.max_files);
        let dirty = self.dirty.clone();
        walking.store(true, Ordering::Relaxed);
        if dirs {
            // the root itself, so it can be picked (e.g. as a move target)
            inj.push("./".to_string(), |s, cols| cols[0] = s.as_str().into());
        }
        std::thread::spawn(move || {
            let mut wb = ignore::WalkBuilder::new(&root);
            wb.hidden(!hidden)
                .git_ignore(!no_ignore)
                .git_global(!no_ignore)
                .git_exclude(!no_ignore)
                .ignore(!no_ignore)
                .parents(!no_ignore)
                .require_git(false)
                .follow_links(false)
                .filter_entry(|e| e.file_name() != ".git");
            let walker = wb.build_parallel();
            walker.run(|| {
                let inj = inj.clone();
                let root = root.clone();
                let cancel = cancel.clone();
                let walked = walked.clone();
                Box::new(move |res| {
                    if cancel.load(Ordering::Relaxed) || walked.load(Ordering::Relaxed) >= cap {
                        return ignore::WalkState::Quit;
                    }
                    let Ok(e) = res else { return ignore::WalkState::Continue };
                    if e.depth() == 0 {
                        return ignore::WalkState::Continue;
                    }
                    let is_dir = e.file_type().is_some_and(|t| t.is_dir());
                    if is_dir != dirs {
                        return ignore::WalkState::Continue;
                    }
                    if let Ok(rel) = e.path().strip_prefix(&root) {
                        let mut s = rel.to_string_lossy().into_owned();
                        if is_dir {
                            s.push('/');
                        }
                        walked.fetch_add(1, Ordering::Relaxed);
                        inj.push(s, |s, cols| cols[0] = s.as_str().into());
                    }
                    ignore::WalkState::Continue
                })
            });
            walking.store(false, Ordering::Relaxed);
            dirty.store(true, Ordering::Relaxed);
        });
    }

    fn reparse(&mut self, append: bool) {
        self.nucleo.pattern.reparse(0, &self.query, CaseMatching::Smart, Normalization::Smart, append);
    }

    pub fn query_changed(&mut self, appended: bool) {
        self.sel = 0;
        self.scroll = 0;
        if self.kind == FinderKind::Grep {
            self.grep.changed_at = Instant::now();
        } else {
            self.reparse(appended);
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Drive background work; true when the view should redraw.
    pub fn tick(&mut self) -> bool {
        let mut changed = self.dirty.swap(false, Ordering::Relaxed);
        if self.kind == FinderKind::Grep {
            let due = self.grep.changed_at.elapsed() > Duration::from_millis(140);
            if due && self.grep.launched.as_deref() != Some(self.query.as_str()) {
                self.launch_grep();
                changed = true;
            }
            if self.grep.running.load(Ordering::Relaxed) {
                changed = true;
            }
        } else {
            let st = self.nucleo.tick(10);
            changed |= st.changed || st.running || self.walking.load(Ordering::Relaxed);
        }
        changed
    }

    fn launch_grep(&mut self) {
        self.grep.cancel.store(true, Ordering::Relaxed);
        if let Ok(mut c) = self.grep.child.lock() {
            if let Some(mut ch) = c.take() {
                let _ = ch.kill();
                let _ = ch.wait();
            }
        }
        if let Ok(mut h) = self.grep.hits.lock() {
            h.clear();
        }
        self.grep.truncated.store(false, Ordering::Relaxed);
        let q = self.query.clone();
        self.grep.launched = Some(q.clone());
        let (pat, filters) = grep_query(&q);
        if pat.trim().is_empty() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.grep.cancel = cancel.clone();
        let mut cmd;
        let rg = rg_available();
        if rg {
            cmd = Command::new("rg");
            cmd.args(["--line-number", "--no-heading", "--color=never", "--smart-case", "--max-columns=300", "--max-columns-preview", "--max-count=200", "--glob=!.git"]);
            if self.hidden {
                cmd.arg("--hidden");
            }
            if self.no_ignore {
                cmd.arg("--no-ignore");
            }
            cmd.args(&filters).arg("--").arg(&pat).arg(".");
        } else {
            cmd = Command::new("grep");
            cmd.args(["-rnI", "--exclude-dir=.git", "-i"]);
            cmd.args(filters.iter().filter_map(|f| f.strip_prefix("--glob=")).map(|g| format!("--include={g}")));
            cmd.arg("-e").arg(&pat).arg(".");
        }
        cmd.current_dir(&self.root).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let Ok(mut child) = cmd.spawn() else { return };
        let stdout = child.stdout.take();
        if let Ok(mut c) = self.grep.child.lock() {
            *c = Some(child);
        }
        let hits = self.grep.hits.clone();
        let running = self.grep.running.clone();
        let truncated = self.grep.truncated.clone();
        let childslot = self.grep.child.clone();
        let root = self.root.clone();
        let dirty = self.dirty.clone();
        running.store(true, Ordering::Relaxed);
        std::thread::spawn(move || {
            if let Some(out) = stdout {
                let mut batch = Vec::new();
                let mut last = Instant::now();
                for line in BufReader::new(out).lines() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(line) = line else { break };
                    if let Some(h) = parse_hit(&line, &root) {
                        batch.push(h);
                    }
                    if batch.len() >= 64 || last.elapsed() > Duration::from_millis(50) {
                        let mut hs = hits.lock().unwrap();
                        hs.append(&mut batch);
                        last = Instant::now();
                        dirty.store(true, Ordering::Relaxed);
                        if hs.len() >= GREP_CAP {
                            truncated.store(true, Ordering::Relaxed);
                            break;
                        }
                    }
                }
                if !cancel.load(Ordering::Relaxed) {
                    hits.lock().unwrap().append(&mut batch);
                }
            }
            if !cancel.load(Ordering::Relaxed) {
                if let Ok(mut c) = childslot.lock() {
                    if let Some(mut ch) = c.take() {
                        let _ = ch.kill();
                        let _ = ch.wait();
                    }
                }
                running.store(false, Ordering::Relaxed);
            }
            dirty.store(true, Ordering::Relaxed);
        });
    }

    pub fn len(&self) -> usize {
        match self.kind {
            FinderKind::Grep => self.grep.hits.lock().map(|h| h.len()).unwrap_or(0),
            _ => self.nucleo.snapshot().matched_item_count() as usize,
        }
    }

    /// Items in [start, end) with highlight indices.
    pub fn items(&mut self, start: usize, end: usize) -> Vec<Item> {
        let n = self.len();
        let end = end.min(n);
        if start >= end {
            return vec![];
        }
        if self.kind == FinderKind::Grep {
            let hs = self.grep.hits.lock().unwrap();
            return hs[start..end]
                .iter()
                .map(|h| {
                    let rel = h.path.strip_prefix(&self.root).unwrap_or(&h.path).display().to_string();
                    Item { display: rel, abs: h.path.clone(), line: Some(h.line), indices: vec![], is_dir: false, grep_text: Some(h.text.clone()), git: None, tag: None, proj: None }
                })
                .collect();
        }
        let rows = self.projects.clone();
        let rows = rows.lock().ok().filter(|_| self.kind == FinderKind::Projects);
        let snap = self.nucleo.snapshot();
        let pat = snap.pattern().column_pattern(0);
        let mut out = Vec::with_capacity(end - start);
        for it in snap.matched_items(start as u32..end as u32) {
            let mut idx = Vec::new();
            pat.indices(it.matcher_columns[0].slice(..), &mut self.matcher, &mut idx);
            idx.sort_unstable();
            idx.dedup();
            let s = it.data.clone();
            let dash = self.dash.get(&s).filter(|_| self.kind == FinderKind::Dash);
            let proj = rows.as_ref().and_then(|m| m.get(&s).cloned());
            let abs = if let Some(p) = &proj {
                p.path.clone()
            } else if let Some((_, p)) = dash {
                p.clone()
            } else if self.kind == FinderKind::Recent {
                expand_tilde(&s)
            } else {
                plan_normalize(&self.root.join(s.trim_end_matches('/')))
            };
            let git = self.git.as_ref().filter(|_| matches!(self.kind, FinderKind::Git | FinderKind::Dash)).and_then(|m| m.get(&s).copied());
            let tag = dash.map(|(t, _)| *t).filter(|_| git.is_none());
            out.push(Item { is_dir: s.ends_with('/') || proj.is_some(), display: s, abs, line: None, indices: idx, grep_text: None, git, tag, proj });
        }
        out
    }

    pub fn selected(&mut self) -> Option<Item> {
        let s = self.sel;
        self.items(s, s + 1).into_iter().next()
    }

    pub fn move_sel(&mut self, d: isize) {
        let n = self.len();
        if n == 0 {
            self.sel = 0;
            return;
        }
        self.sel = (self.sel as isize + d).clamp(0, n as isize - 1) as usize;
    }

    pub fn follow(&mut self, h: usize) {
        let h = h.max(1);
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + h {
            self.scroll = self.sel + 1 - h;
        }
    }

    pub fn toggle_mark(&mut self) {
        if let Some(it) = self.selected() {
            if it.is_dir {
                return;
            }
            let m = Mark { path: it.abs, line: it.line };
            if let Some(i) = self.marked.iter().position(|x| *x == m) {
                self.marked.remove(i);
            } else {
                self.marked.push(m);
            }
        }
    }
}

impl Drop for Finder {
    fn drop(&mut self) {
        self.stop();
    }
}

fn plan_normalize(p: &Path) -> PathBuf {
    crate::plan::normalize(p)
}

fn parse_hit(line: &str, root: &Path) -> Option<Hit> {
    // path:line:text — path may contain ':' so split from the left on the
    // first ":<digits>:".
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let rest = &line[i + 1..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() && rest[digits.len()..].starts_with(':') {
                let path = &line[..i];
                let path = path.strip_prefix("./").unwrap_or(path);
                let text = &rest[digits.len() + 1..];
                return Some(Hit { path: root.join(path), line: digits.parse().ok()?, text: text.trim_end().replace('\t', "  ") });
            }
        }
        i += 1;
    }
    None
}

fn expand_tilde(s: &str) -> PathBuf {
    match (s.strip_prefix('~'), std::env::var("HOME")) {
        (Some(rest), Ok(h)) => PathBuf::from(format!("{h}{rest}")),
        _ => PathBuf::from(s),
    }
}

pub fn read_recent(f: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(f).unwrap_or_default().lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect()
}

pub fn push_recent(state_dir: &Path, paths: &[PathBuf]) {
    let f = state_dir.join("recent.txt");
    let mut v = read_recent(&f);
    for p in paths.iter().rev() {
        v.retain(|x| x != p);
        v.insert(0, p.clone());
    }
    v.truncate(300);
    let _ = std::fs::create_dir_all(state_dir);
    let body: String = v.iter().map(|p| format!("{}\n", p.display())).collect();
    let _ = std::fs::write(f, body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_tab_lists_changes() {
        let t = crate::plan::tests::tmpdir("finder-git");
        let git = |args: &[&str]| Command::new("git").args(args).current_dir(&t.0).output().unwrap();
        git(&["init", "-q"]);
        std::fs::write(t.0.join("new.rs"), "x").unwrap();
        let cfg = crate::config::Config::default();
        let mut f = Finder::new(FinderKind::Git, t.0.clone(), &cfg);
        let t0 = Instant::now();
        while f.len() < 1 && t0.elapsed() < Duration::from_secs(5) {
            f.tick();
            std::thread::sleep(Duration::from_millis(10));
        }
        let it = f.selected().unwrap();
        assert_eq!((it.display.as_str(), it.git.map(|g| g.letter)), ("new.rs", Some('?')));
    }

    #[test]
    fn dashboard_merges_sources() {
        let t = crate::plan::tests::tmpdir("finder-dash");
        Command::new("git").args(["init", "-q"]).current_dir(&t.0).output().unwrap();
        std::fs::write(t.0.join("changed.rs"), "x").unwrap();
        std::fs::write(t.0.join("old.txt"), "x").unwrap();
        let cfg = crate::config::Config { state_dir: t.0.join(".state"), ..Default::default() };
        push_recent(&cfg.state_dir, &[t.0.join("old.txt"), t.0.join("changed.rs")]);
        let mut f = Finder::new(FinderKind::Files, t.0.clone(), &cfg);
        f.pending = vec![('+', t.0.join("new.md"))];
        f.set_kind(FinderKind::Dash);
        for _ in 0..50 {
            f.tick();
        }
        let items = f.items(0, 10);
        let got: Vec<(String, Option<char>, Option<char>)> = items.iter().map(|i| (i.display.clone(), i.tag, i.git.map(|g| g.letter))).collect();
        assert!(got.contains(&("new.md".into(), Some('+'), None)), "{got:?}");
        assert!(got.contains(&("changed.rs".into(), None, Some('?'))), "{got:?}");
        assert_eq!(got.iter().filter(|g| g.0 == "changed.rs").count(), 1, "listed once");
    }

    #[test]
    fn projects_tab_lists_and_scopes() {
        let t = crate::plan::tests::tmpdir("finder-proj");
        for n in ["clean", "busy", "plain"] {
            std::fs::create_dir_all(t.0.join(n)).unwrap();
        }
        let git = |d: &str, args: &[&str]| Command::new("git").args(args).current_dir(t.0.join(d)).output().unwrap();
        git("clean", &["init", "-q", "-b", "trunk"]);
        git("busy", &["init", "-q", "-b", "wip"]);
        std::fs::write(t.0.join("busy/x.txt"), "x").unwrap();
        let mut cfg = crate::config::Config { state_dir: t.0.join(".state"), ..Default::default() };
        cfg.projects.roots = vec![t.0.clone()];
        cfg.projects.recent_file = t.0.join(".state/recent.tsv");
        projects::bump_recent(&cfg.projects.recent_file, &t.0.join("busy"));
        let mut f = Finder::new(FinderKind::Projects, t.0.join("clean"), &cfg);
        let settle = |f: &mut Finder| {
            for _ in 0..400 {
                f.tick();
                if !f.walking() {
                    for _ in 0..20 {
                        f.tick();
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        settle(&mut f);
        let items = f.items(0, 10);
        assert_eq!(items.len(), 3);
        let first = items[0].proj.as_ref().unwrap();
        assert_eq!((first.name.as_str(), first.dirty), ("busy", Some((1, 0))), "recent first, dirty counted");
        assert!(items.iter().all(|i| i.is_dir));
        assert!(items.iter().any(|i| i.proj.as_ref().is_some_and(|p| p.name == "clean" && p.here)));
        // branch names are searchable
        f.query = "trunk".into();
        f.query_changed(true);
        settle(&mut f);
        assert_eq!(f.items(0, 10).iter().map(|i| i.proj.as_ref().unwrap().name.clone()).collect::<Vec<_>>(), ["clean"]);
        f.query.clear();
        f.query_changed(false);
        // all → open → dirty
        f.cycle_scope();
        f.cycle_scope();
        assert_eq!(f.proj_scope, Scope::Dirty);
        settle(&mut f);
        assert_eq!(f.items(0, 10).iter().map(|i| i.abs.clone()).collect::<Vec<_>>(), [t.0.join("busy")]);
    }

    #[test]
    fn grep_filters() {
        assert_eq!(grep_query("foo"), ("foo".to_string(), vec![]));
        assert_eq!(
            grep_query("fn main -- *.rs -t py -Tmd --exec x"),
            ("fn main".to_string(), vec!["--glob=*.rs".to_string(), "--type=py".to_string(), "--type-not=md".to_string(), "--glob=x".to_string()])
        );
    }

    #[test]
    fn parses_grep_lines() {
        let h = parse_hit("./src/a:b.rs:12:  let x = 1;", Path::new("/r")).unwrap();
        assert_eq!(h.path, PathBuf::from("/r/src/a:b.rs"));
        assert_eq!(h.line, 12);
        assert_eq!(h.text, "  let x = 1;");
        assert!(parse_hit("nonsense", Path::new("/r")).is_none());
    }

    #[test]
    fn recent_roundtrip() {
        let d = std::env::temp_dir().join(format!("herdr-oil-recent-{}", std::process::id()));
        push_recent(&d, &[PathBuf::from("/a"), PathBuf::from("/b")]);
        push_recent(&d, &[PathBuf::from("/b")]);
        assert_eq!(read_recent(&d.join("recent.txt")), [PathBuf::from("/b"), PathBuf::from("/a")]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn walks_and_matches() {
        let t = crate::plan::tests::tmpdir("finder");
        std::fs::create_dir_all(t.0.join("src/deep")).unwrap();
        std::fs::write(t.0.join("src/deep/needle.rs"), "fn main() { haystack }").unwrap();
        std::fs::write(t.0.join("README.md"), "").unwrap();
        let cfg = crate::config::Config::default();
        let mut f = Finder::new(FinderKind::Files, t.0.clone(), &cfg);
        let t0 = Instant::now();
        while (f.walking() || f.total() < 2) && t0.elapsed() < Duration::from_secs(5) {
            f.tick();
            std::thread::sleep(Duration::from_millis(10));
        }
        f.query = "ndl".into();
        f.query_changed(false);
        for _ in 0..50 {
            f.tick();
        }
        let items = f.items(0, 10);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].display, "src/deep/needle.rs");
        assert!(!items[0].indices.is_empty());

        f.set_kind(FinderKind::Grep);
        f.query = "haystack".into();
        f.query_changed(true);
        std::thread::sleep(Duration::from_millis(200));
        let t0 = Instant::now();
        while f.len() == 0 && t0.elapsed() < Duration::from_secs(5) {
            f.tick();
            std::thread::sleep(Duration::from_millis(20));
        }
        let g = f.items(0, 5);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].line, Some(1));
    }

    #[test]
    fn git_tab_roots_at_repo() {
        let t = crate::plan::tests::tmpdir("finder-git-root");
        Command::new("git").args(["init", "-q"]).current_dir(&t.0).output().unwrap();
        let sub = t.0.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let cfg = crate::config::Config::default();
        let mut f = Finder::new(FinderKind::Files, sub.clone(), &cfg);
        f.set_kind(FinderKind::Git);
        assert_eq!(f.root, t.0);
        f.set_kind(FinderKind::Files);
        assert_eq!(f.root, sub);
        assert_eq!(Finder::new(FinderKind::Git, sub, &cfg).root, t.0);
    }
}
