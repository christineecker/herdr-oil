//! Projects (the finder's Projects tab): every project under the configured
//! roots, recent hops first, with branch, open-workspace and agent state;
//! `⏎` hops to it as a herdr workspace. Shares herdr-hop's config (`roots`,
//! `max_depth`, …) and its `recent.tsv`, so both plugins rank alike.

use crate::entry::IconColor;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub roots: Vec<PathBuf>,
    /// How deep below a root to look for `.git`; depth-1 folders always count.
    pub max_depth: usize,
    pub recent_limit: usize,
    /// Where `^n` creates projects.
    pub default_root: PathBuf,
    pub default_branch: String,
    /// `epoch<TAB>path`, newest first (herdr-hop's file when it is installed).
    pub recent_file: PathBuf,
}

impl Settings {
    pub fn new(state_dir: &Path) -> Settings {
        let home = home();
        Settings {
            roots: vec![home.join("Projects")],
            max_depth: 2,
            recent_limit: 12,
            default_root: home.join("Projects"),
            default_branch: "main".into(),
            recent_file: hop_state(state_dir).unwrap_or_else(|| state_dir.to_path_buf()).join("recent.tsv"),
        }
    }

    /// Keys of herdr-hop's `config.toml` or herdr-oil's `[projects]` table.
    pub fn apply(&mut self, t: &toml::Table) {
        let s = |k: &str| t.get(k).and_then(|v| v.as_str()).map(expand);
        let n = |k: &str| t.get(k).and_then(|v| v.as_integer()).filter(|n| *n > 0).map(|n| n as usize);
        if let Some(a) = t.get("roots").and_then(|v| v.as_array()) {
            let roots: Vec<PathBuf> = a.iter().filter_map(|v| v.as_str()).map(expand).collect();
            if !roots.is_empty() {
                self.roots = roots;
            }
        }
        if let Some(v) = n("max_depth") {
            self.max_depth = v.min(6);
        }
        if let Some(v) = n("recent_limit") {
            self.recent_limit = v;
        }
        if let Some(v) = s("default_root") {
            self.default_root = v;
        }
        if let Some(v) = t.get("default_branch").and_then(|v| v.as_str()).filter(|b| valid_name(b)) {
            self.default_branch = v.to_string();
        }
        if let Some(v) = s("recent_file") {
            self.recent_file = v;
        }
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

fn expand(s: &str) -> PathBuf {
    match s.strip_prefix('~') {
        Some(rest) => PathBuf::from(format!("{}{rest}", home().display())),
        None => PathBuf::from(s),
    }
}

/// herdr-hop's state folder: next to ours under herdr's plugin state dir,
/// else its default location. `None` when hop never ran.
fn hop_state(state_dir: &Path) -> Option<PathBuf> {
    let sibling = state_dir.parent().map(|p| p.join("herdr-hop"));
    let default = home().join(".local/state/herdr/plugins/herdr-hop");
    sibling.into_iter().chain([default]).find(|d| d.is_dir())
}

/// herdr-hop's config file, next to ours.
pub fn hop_config(config_dir: &Path) -> Option<PathBuf> {
    Some(config_dir.parent()?.join("herdr-hop/config.toml")).filter(|p| p.is_file())
}

pub fn valid_name(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

// ── recent ──────────────────────────────────────────────────────────────────

pub fn read_recent(f: &Path) -> Vec<(u64, PathBuf)> {
    std::fs::read_to_string(f)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (ts, p) = l.split_once('\t')?;
            Some((ts.trim().parse().ok()?, PathBuf::from(p)))
        })
        .collect()
}

fn write_recent(f: &Path, v: &[(u64, PathBuf)]) {
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let body: String = v.iter().take(200).map(|(t, p)| format!("{t}\t{}\n", p.display())).collect();
    let tmp = f.with_extension("tsv.oil-tmp");
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, f);
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn bump_recent(f: &Path, p: &Path) {
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let mut v = read_recent(f);
    v.retain(|(_, x)| *x != p);
    v.insert(0, (now(), p));
    write_recent(f, &v);
}

pub fn forget_recent(f: &Path, p: &Path) {
    let mut v = read_recent(f);
    v.retain(|(_, x)| x != p);
    write_recent(f, &v);
}

// ── index ───────────────────────────────────────────────────────────────────

/// Folders under the roots: every non-hidden depth-1 folder, plus any
/// folder with a `.git` (directory, or file for linked worktrees) down to
/// `max_depth` levels. Found by reading directories, without running git.
pub fn index(s: &Settings) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    fn walk(d: &Path, depth: usize, max: usize, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        let mut kids: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| e.path())
            .collect();
        kids.sort();
        for k in kids {
            let repo = std::fs::symlink_metadata(k.join(".git")).is_ok();
            if (depth == 1 || repo) && seen.insert(k.clone()) {
                out.push(k.clone());
            }
            // don't descend into a repository's own tree (node_modules, target, …)
            if depth < max && !repo {
                walk(&k, depth + 1, max, out, seen);
            }
        }
    }
    for r in &s.roots {
        walk(r, 1, s.max_depth.max(1), &mut out, &mut seen);
    }
    out
}

/// Stack icon from marker files, as herdr-hop picks them.
pub fn icon(p: &Path) -> (&'static str, IconColor) {
    let has = |f: &str| p.join(f).exists();
    if p.join(".git").is_file() {
        ("\u{f0645}", IconColor::Teal)
    } else if has("Cargo.toml") {
        ("\u{e7a8}", IconColor::Peach)
    } else if has("pyproject.toml") {
        ("\u{e73c}", IconColor::Yellow)
    } else if has("Package.swift") {
        ("\u{e755}", IconColor::Peach)
    } else if has("package.json") {
        ("\u{e718}", IconColor::Green)
    } else if has("index.md") || has("wiki.md") || (p.join("raw").is_dir() && p.join("wiki").is_dir()) {
        ("\u{f00ba}", IconColor::Blue)
    } else {
        ("\u{f024b}", IconColor::Accent)
    }
}

/// HEAD read from disk: branch (or short commit when detached) and, for a
/// linked worktree, the name of the repository it belongs to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Head {
    pub name: String,
    pub detached: bool,
    pub worktree_of: Option<String>,
}

pub fn head(p: &Path) -> Option<Head> {
    let git = p.join(".git");
    let mut h = Head::default();
    let gd = if git.is_file() {
        let text = std::fs::read_to_string(&git).ok()?;
        let g = PathBuf::from(text.lines().next()?.strip_prefix("gitdir:")?.trim());
        let g = if g.is_absolute() { g } else { p.join(g) };
        // <repo>/.git/worktrees/<name>
        let s = g.to_string_lossy();
        if let Some(i) = s.find("/.git/worktrees/") {
            h.worktree_of = Path::new(&s[..i]).file_name().map(|n| n.to_string_lossy().into_owned());
        }
        g
    } else {
        git
    };
    let r = std::fs::read_to_string(gd.join("HEAD")).ok()?;
    let r = r.trim();
    match r.strip_prefix("ref: ") {
        Some(rf) => h.name = rf.strip_prefix("refs/heads/").unwrap_or(rf).to_string(),
        None => {
            h.name = r.chars().take(7).collect();
            h.detached = true;
        }
    }
    Some(h)
}

// ── herdr ───────────────────────────────────────────────────────────────────

/// An open herdr workspace with a pane inside a project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ws {
    pub id: String,
    pub number: u64,
    pub label: String,
    pub tabs: u64,
    pub panes: u64,
    pub agents: usize,
    /// `working`, `waiting`, `idle` or empty (no agent).
    pub status: String,
    /// The tab holding the project's first pane (⏎ jumps to it).
    pub tab: String,
    /// Every pane of the workspace sits in this project (only then may
    /// `^k` close the whole workspace).
    pub whole: bool,
    /// Every pane of `tab` sits in this project.
    pub tab_whole: bool,
}

/// Open workspaces per project: a pane belongs to the innermost project
/// that contains its cwd. Empty when herdr is not reachable.
pub fn workspaces(projects: &HashSet<PathBuf>) -> HashMap<PathBuf, Ws> {
    let mut out: HashMap<PathBuf, Ws> = HashMap::new();
    let json = |args: &[&str]| crate::launch::herdr(args).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(s.trim_start_matches('\u{feff}')).ok());
    let (Some(wl), Some(pl)) = (json(&["workspace", "list"]), json(&["pane", "list"])) else { return out };
    let wss = wl.pointer("/result/workspaces").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let panes = pl.pointer("/result/panes").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let str_of = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    // (project, workspace) → agent statuses
    // (project, workspace) → (agent statuses, first tab, panes there); panes per workspace / tab
    type Seen = HashMap<(PathBuf, String), (Vec<String>, String, usize)>;
    let mut seen: Seen = HashMap::new();
    let mut per_ws: HashMap<String, usize> = HashMap::new();
    let mut per_tab: HashMap<String, usize> = HashMap::new();
    let mut proj_tab: HashMap<(PathBuf, String), usize> = HashMap::new();
    for p in &panes {
        let ws = str_of(p, "workspace_id");
        let tab = str_of(p, "tab_id");
        *per_ws.entry(ws.clone()).or_default() += 1;
        *per_tab.entry(tab.clone()).or_default() += 1;
        let cwd = p.get("foreground_cwd").and_then(|x| x.as_str()).or_else(|| p.get("cwd").and_then(|x| x.as_str()));
        let Some(cwd) = cwd else { continue };
        let Some(proj) = Path::new(cwd).ancestors().find(|a| projects.contains(*a)) else { continue };
        *proj_tab.entry((proj.to_path_buf(), tab.clone())).or_default() += 1;
        let e = seen.entry((proj.to_path_buf(), ws)).or_insert_with(|| (vec![], tab, 0));
        e.2 += 1;
        if p.get("agent").is_some_and(|a| !a.is_null()) {
            e.0.push(str_of(p, "agent_status"));
        }
    }
    for ((proj, id), (st, tab, n)) in seen {
        let whole = per_ws.get(&id) == Some(&n);
        let tab_whole = per_tab.get(&tab) == proj_tab.get(&(proj.clone(), tab.clone()));
        let w = wss.iter().find(|w| str_of(w, "workspace_id") == id);
        let num = |k: &str| w.and_then(|w| w.get(k)).and_then(|x| x.as_u64()).unwrap_or(0);
        let status = if st.iter().any(|s| s.contains("wait") || s.contains("block") || s.contains("input")) {
            "waiting"
        } else if st.iter().any(|s| s == "working") {
            "working"
        } else if !st.is_empty() {
            "idle"
        } else {
            ""
        };
        let ws = Ws { id, number: num("number"), label: w.map(|w| str_of(w, "label")).unwrap_or_default(), tabs: num("tab_count"), panes: num("pane_count"), agents: st.len(), status: status.into(), tab, whole, tab_whole };
        // several workspaces on one project: keep the busiest
        match out.get(&proj) {
            Some(o) if o.agents >= ws.agents => {}
            _ => {
                out.insert(proj, ws);
            }
        }
    }
    out
}

fn focused_workspace() -> Option<String> {
    if let Some(w) = std::env::var("HERDR_WORKSPACE_ID").ok().filter(|w| crate::launch::flag_safe(w)) {
        return Some(w);
    }
    let v: serde_json::Value = serde_json::from_str(&crate::launch::herdr(&["workspace", "list"]).ok()?).ok()?;
    v.pointer("/result/workspaces")?.as_array()?.iter().find(|w| w.get("focused").and_then(|f| f.as_bool()) == Some(true))?.get("workspace_id")?.as_str().map(str::to_string)
}

/// Workspace / tab label: the folder name, `name (parent)` when another
/// project has the same name.
pub fn label(p: &Path, all: &[PathBuf]) -> String {
    let base = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
    let twins = all.iter().filter(|q| q.file_name() == p.file_name()).count();
    match p.parent().and_then(|d| d.file_name()) {
        Some(par) if twins > 1 => format!("{base} ({})", par.to_string_lossy()),
        _ => base,
    }
}

/// Hop: focus the workspace already open on the project, else create one
/// (`tab`: a tab in the current workspace instead). Records the hop.
pub fn hop(p: &Path, tab: bool, ws: Option<&Ws>, label: &str, s: &Settings) -> Result<String, String> {
    let dir = p.to_string_lossy();
    if label.chars().any(char::is_control) || dir.chars().any(char::is_control) {
        return Err("name contains control characters".into());
    }
    let msg = if tab {
        let w = focused_workspace().ok_or("no focused workspace")?;
        crate::launch::herdr(&["tab", "create", "--workspace", &w, "--cwd", &dir, "--label", label, "--focus"])?;
        format!("opened {label} in a new tab")
    } else if let Some(ws) = ws.filter(|w| crate::launch::flag_safe(&w.id)) {
        crate::launch::herdr(&["workspace", "focus", &ws.id])?;
        if crate::launch::flag_safe(&ws.tab) {
            let _ = crate::launch::herdr(&["tab", "focus", &ws.tab]);
        }
        format!("jumped to {label} in workspace {}", ws.number)
    } else {
        crate::launch::herdr(&["workspace", "create", "--cwd", &dir, "--label", label, "--focus"])?;
        format!("created workspace {label}")
    };
    bump_recent(&s.recent_file, p);
    Ok(msg)
}

/// What `^k` would close: the workspace when the project owns all of it,
/// else the project's tab when it owns that, else nothing (panes of other
/// projects share it).
pub fn close_target(ws: &Ws) -> Option<(&'static str, &str)> {
    if ws.whole && crate::launch::flag_safe(&ws.id) {
        Some(("workspace", &ws.id))
    } else if ws.tab_whole && crate::launch::flag_safe(&ws.tab) {
        Some(("tab", &ws.tab))
    } else {
        None
    }
}

pub fn close(ws: &Ws) -> Result<&'static str, String> {
    let (what, id) = close_target(ws).ok_or("other projects share its workspace and tab — close it in herdr")?;
    crate::launch::herdr(&[what, "close", id]).map(|_| what)
}

/// `^n`: `default_root/name`, `git init -b <branch>`, a README, first commit.
pub fn create(name: &str, s: &Settings) -> Result<PathBuf, String> {
    let name = name.trim().replace(' ', "-");
    if !valid_name(&name) {
        return Err("project name: letters, digits, . _ - only".into());
    }
    let dest = s.default_root.join(&name);
    if dest.exists() {
        return Err(format!("{} exists", crate::finder::tilde(&dest)));
    }
    std::fs::create_dir_all(&dest).map_err(|e| format!("mkdir: {e}"))?;
    let git = |args: &[&str]| -> Result<(), String> {
        let out = Command::new("git").args(args).current_dir(&dest).stdin(Stdio::null()).output().map_err(|e| format!("git: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("git failed").to_string())
        }
    };
    git(&["init", "-q", "-b", &s.default_branch])?;
    std::fs::write(dest.join("README.md"), format!("# {name}\n")).map_err(|e| format!("README: {e}"))?;
    git(&["add", "-A"])?;
    // hooks off: nothing in a fresh repository should run
    git(&["-c", "core.hooksPath=/dev/null", "commit", "-q", "--no-gpg-sign", "-m", "init"])?;
    Ok(std::fs::canonicalize(&dest).unwrap_or(dest))
}

// ── rows ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    Open,
    Recent,
    /// Uncommitted changes or commits ahead of the upstream.
    Dirty,
}

impl Scope {
    pub fn next(self) -> Scope {
        match self {
            Scope::All => Scope::Open,
            Scope::Open => Scope::Dirty,
            Scope::Dirty => Scope::Recent,
            Scope::Recent => Scope::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "all",
            Scope::Open => "open",
            Scope::Recent => "recent",
            Scope::Dirty => "dirty",
        }
    }
}

/// One row of the Projects tab.
#[derive(Clone, Debug)]
pub struct Proj {
    pub path: PathBuf,
    /// Relative to its root (or `~/…`).
    pub name: String,
    /// Workspace / tab label.
    pub label: String,
    /// Last hop (epoch), for rows in the RECENT section.
    pub hopped: Option<u64>,
    pub icon: (&'static str, IconColor),
    pub head: Option<Head>,
    pub ws: Option<Ws>,
    /// The explorer's start directory is inside this project.
    pub here: bool,
    /// Changed files and commits ahead, once the background check is in.
    pub dirty: Option<(usize, usize)>,
}

/// Display name: relative to the root that holds it, else `~/…`.
pub fn display_name(p: &Path, roots: &[PathBuf]) -> String {
    roots
        .iter()
        .find_map(|r| p.strip_prefix(r).ok().filter(|x| !x.as_os_str().is_empty()))
        .map(|x| x.display().to_string())
        .unwrap_or_else(|| crate::finder::tilde(p))
}

/// The rows in display order: RECENT (up to `recent_limit`), then open
/// workspaces, then the rest by modification time. `here` is the folder
/// oil was opened in.
pub fn collect(s: &Settings, scope: Scope, here: &Path) -> Vec<Proj> {
    let all = index(s);
    let recent = read_recent(&s.recent_file);
    let mut set: HashSet<PathBuf> = all.iter().cloned().collect();
    // recent projects outside the roots still count
    for (_, p) in recent.iter().take(s.recent_limit) {
        if p.is_dir() {
            set.insert(p.clone());
        }
    }
    let ws = workspaces(&set);
    let here = std::fs::canonicalize(here).unwrap_or_else(|_| here.to_path_buf());
    let inner = here.ancestors().find(|a| set.contains(*a)).map(Path::to_path_buf);
    let row = |p: &Path, hopped: Option<u64>| Proj {
        path: p.to_path_buf(),
        name: display_name(p, &s.roots),
        label: label(p, &all),
        hopped,
        icon: icon(p),
        head: head(p),
        ws: ws.get(p).cloned(),
        here: inner.as_deref() == Some(p),
        dirty: None,
    };
    let mut out = Vec::new();
    let mut taken = HashSet::new();
    for (ts, p) in recent.iter().filter(|(_, p)| p.is_dir()).take(s.recent_limit) {
        if taken.insert(p.clone()) {
            out.push(row(p, Some(*ts)));
        }
    }
    if scope != Scope::Recent {
        let mut open: Vec<&PathBuf> = ws.keys().filter(|p| !taken.contains(*p)).collect();
        open.sort();
        for p in open {
            taken.insert(p.clone());
            out.push(row(p, None));
        }
        let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let mut rest: Vec<&PathBuf> = all.iter().filter(|p| !taken.contains(*p)).collect();
        rest.sort_by_key(|p| std::cmp::Reverse(mtime(p)));
        for p in rest {
            out.push(row(p, None));
        }
    }
    if scope == Scope::Open {
        out.retain(|r| r.ws.is_some());
    }
    out
}

/// Changed files and commits ahead (`git status -b`, with the usual
/// safety knobs and the filter guard). `None` outside a repository.
pub fn dirty(p: &Path) -> Option<(usize, usize)> {
    if std::fs::symlink_metadata(p.join(".git")).is_err() {
        return None;
    }
    let (branch, files) = crate::entry::repo_status(p)?;
    Some((files.len(), branch.map(|b| b.ahead).unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(d: &Path, args: &[&str]) {
        Command::new("git").args(args).current_dir(d).output().unwrap();
    }

    #[test]
    fn index_finds_depth_one_and_nested_repos() {
        let t = crate::plan::tests::tmpdir("proj-index");
        let r = &t.0;
        std::fs::create_dir_all(r.join("plain")).unwrap();
        std::fs::create_dir_all(r.join("group/repo/.git")).unwrap();
        std::fs::create_dir_all(r.join("group/notrepo")).unwrap();
        std::fs::create_dir_all(r.join("a/b/c/deep/.git")).unwrap();
        std::fs::create_dir_all(r.join("top/.git")).unwrap();
        std::fs::create_dir_all(r.join("top/inner/.git")).unwrap();
        std::fs::create_dir_all(r.join(".hidden")).unwrap();
        let mut s = Settings::new(r);
        s.roots = vec![r.clone()];
        let got: Vec<String> = index(&s).iter().map(|p| p.strip_prefix(r).unwrap().display().to_string()).collect();
        assert!(got.contains(&"plain".into()));
        assert!(got.contains(&"group".into()));
        assert!(got.contains(&"group/repo".into()));
        assert!(!got.contains(&"group/notrepo".into()));
        assert!(!got.iter().any(|g| g.contains("deep")), "beyond max_depth");
        assert!(!got.contains(&"top/inner".into()), "not inside a repository");
        assert!(!got.iter().any(|g| g.contains("hidden")));
    }

    #[test]
    fn recent_roundtrip() {
        let t = crate::plan::tests::tmpdir("proj-recent");
        let f = t.0.join("state/recent.tsv");
        let (a, b) = (t.0.join("a"), t.0.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        bump_recent(&f, &a);
        bump_recent(&f, &b);
        bump_recent(&f, &a);
        let v: Vec<PathBuf> = read_recent(&f).into_iter().map(|(_, p)| p).collect();
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap();
        assert_eq!(v, vec![canon(&a), canon(&b)]);
        forget_recent(&f, &canon(&a));
        assert_eq!(read_recent(&f).len(), 1);
        // hop's format: epoch TAB path
        assert!(std::fs::read_to_string(&f).unwrap().split('\t').next().unwrap().parse::<u64>().is_ok());
    }

    #[test]
    fn head_reads_branch_detached_and_worktrees() {
        let t = crate::plan::tests::tmpdir("proj-head");
        let r = t.0.join("r");
        std::fs::create_dir_all(r.join(".git")).unwrap();
        std::fs::write(r.join(".git/HEAD"), "ref: refs/heads/feat/x\n").unwrap();
        assert_eq!(head(&r).unwrap(), Head { name: "feat/x".into(), detached: false, worktree_of: None });
        std::fs::write(r.join(".git/HEAD"), "0123456789abcdef\n").unwrap();
        assert!(head(&r).unwrap().detached);
        let wt = t.0.join("wt");
        std::fs::create_dir_all(r.join(".git/worktrees/wt")).unwrap();
        std::fs::write(r.join(".git/worktrees/wt/HEAD"), "ref: refs/heads/task\n").unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", r.join(".git/worktrees/wt").display())).unwrap();
        assert_eq!(head(&wt).unwrap(), Head { name: "task".into(), detached: false, worktree_of: Some("r".into()) });
        assert_eq!(head(&t.0), None);
    }

    #[test]
    fn labels_disambiguate_twins() {
        let all = vec![PathBuf::from("/x/a/app"), PathBuf::from("/x/b/app"), PathBuf::from("/x/c/solo")];
        assert_eq!(label(&all[0], &all), "app (a)");
        assert_eq!(label(&all[2], &all), "solo");
    }

    #[test]
    fn settings_from_hop_then_oil() {
        let mut s = Settings::new(Path::new("/nonexistent/state"));
        let hop: toml::Table = "roots = [\"~/P\", \"/w\"]\nmax_depth = 3\nrecent_limit = 5\ndefault_branch = \"trunk\"\n[recipe]\nagent = \"claude\"\n".parse().unwrap();
        s.apply(&hop);
        assert_eq!(s.roots, vec![home().join("P"), PathBuf::from("/w")]);
        assert_eq!((s.max_depth, s.recent_limit, s.default_branch.as_str()), (3, 5, "trunk"));
        let oil: toml::Table = "roots = [\"/only\"]\ndefault_branch = \"-bad\"\n".parse().unwrap();
        s.apply(&oil);
        assert_eq!(s.roots, vec![PathBuf::from("/only")]);
        assert_eq!(s.default_branch, "trunk");
    }

    #[test]
    fn create_inits_and_commits() {
        let t = crate::plan::tests::tmpdir("proj-new");
        let mut s = Settings::new(&t.0);
        s.default_root = t.0.clone();
        s.default_branch = "main".into();
        assert!(create("../evil", &s).is_err());
        assert!(create("-x", &s).is_err());
        let p = create("my app", &s).unwrap();
        assert!(p.ends_with("my-app"));
        assert_eq!(head(&p).unwrap().name, "main");
        assert!(p.join("README.md").is_file());
        assert!(create("my-app", &s).is_err(), "never over an existing folder");
    }

    #[test]
    fn collect_orders_recent_first_and_counts_dirty() {
        let t = crate::plan::tests::tmpdir("proj-collect");
        let root = std::fs::canonicalize(&t.0).unwrap();
        for n in ["alpha", "beta", "gamma"] {
            std::fs::create_dir_all(root.join(n)).unwrap();
        }
        git(&root.join("beta"), &["init", "-q"]);
        std::fs::write(root.join("beta/x.txt"), "x").unwrap();
        let mut s = Settings::new(&root);
        s.roots = vec![root.clone()];
        s.recent_file = root.join("recent.tsv");
        bump_recent(&s.recent_file, &root.join("gamma"));
        let rows = collect(&s, Scope::All, &root.join("beta"));
        assert_eq!(rows[0].name, "gamma");
        assert!(rows[0].hopped.is_some());
        assert_eq!(rows.len(), 3);
        let beta = rows.iter().find(|r| r.name == "beta").unwrap();
        assert!(beta.here);
        assert_eq!(dirty(&beta.path), Some((1, 0)));
        assert_eq!(dirty(&root.join("alpha")), None);
        assert_eq!(collect(&s, Scope::Recent, &root).len(), 1);
    }
}
