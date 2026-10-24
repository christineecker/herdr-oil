//! Directory entries, the id store that ties buffer lines to real files, and
//! the small helpers that describe an entry (icon, size, age, git status).

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Link,
    LinkDir,
    Other,
}

impl Kind {
    pub fn is_dir(self) -> bool {
        matches!(self, Kind::Dir | Kind::LinkDir)
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: u64,
    pub path: PathBuf,
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    pub mtime: Option<SystemTime>,
    pub mode: u32,
    pub link_target: Option<String>,
    /// Direct children of a directory (None for files or unreadable dirs).
    pub children: Option<usize>,
    pub uid: u32,
    pub gid: u32,
}

impl Entry {
    /// Text shown (and edited) in the buffer: dirs get a trailing slash,
    /// symlinks their target (`name -> target`, editable).
    pub fn display(&self) -> String {
        let base = if self.kind.is_dir() { format!("{}/", self.name) } else { self.name.clone() };
        match &self.link_target {
            Some(t) => format!("{base} -> {t}"),
            None => base,
        }
    }
    pub fn is_exec(&self) -> bool {
        self.kind == Kind::File && self.mode & 0o111 != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Name,
    Mtime,
    Size,
    Ext,
}

impl Sort {
    pub fn next(self) -> Sort {
        match self {
            Sort::Name => Sort::Mtime,
            Sort::Mtime => Sort::Size,
            Sort::Size => Sort::Ext,
            Sort::Ext => Sort::Name,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Sort::Name => "name",
            Sort::Mtime => "mtime",
            Sort::Size => "size",
            Sort::Ext => "ext",
        }
    }
}

/// Every entry ever listed gets a stable id. Re-listing the same path keeps
/// the id, so a line yanked before a refresh still points at the same file.
#[derive(Default)]
pub struct Store {
    next: u64,
    /// Reverse the sort order (dirs still come first).
    pub reverse: bool,
    pub entries: HashMap<u64, Entry>,
    by_path: HashMap<PathBuf, u64>,
}

impl Store {
    pub fn get(&self, id: u64) -> Option<&Entry> {
        self.entries.get(&id)
    }

    fn intern(&mut self, mut e: Entry) -> u64 {
        let id = match self.by_path.get(&e.path) {
            Some(&id) => id,
            None => {
                self.next += 1;
                self.by_path.insert(e.path.clone(), self.next);
                self.next
            }
        };
        e.id = id;
        self.entries.insert(id, e);
        id
    }

    /// Forget everything (after changes were applied on disk).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.by_path.clear();
    }

    /// List `dir`, sorted dirs-first, returning ids.
    pub fn list(&mut self, dir: &Path, show_hidden: bool, sort: Sort) -> std::io::Result<Vec<u64>> {
        let mut items = Vec::new();
        for de in fs::read_dir(dir)? {
            let Ok(de) = de else { continue };
            let name = de.file_name().to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            items.push(stat_entry(&de.path(), name));
        }
        sort_entries(&mut items, sort, self.reverse);
        Ok(items.into_iter().map(|e| self.intern(e)).collect())
    }
}

/// Directory child counts stop here and show as "1k+ items".
pub const CHILD_CAP: usize = 1000;

pub fn stat_entry(path: &Path, name: String) -> Entry {
    let lmeta = fs::symlink_metadata(path).ok();
    let mut kind = Kind::Other;
    let mut link_target = None;
    let mut size = 0;
    let mut mtime = None;
    let mut mode = 0;
    let (mut uid, mut gid) = (0, 0);
    if let Some(m) = &lmeta {
        use std::os::unix::fs::MetadataExt;
        (uid, gid) = (m.uid(), m.gid());
        let ft = m.file_type();
        mtime = m.modified().ok();
        mode = m.permissions().mode();
        size = m.len();
        if ft.is_symlink() {
            link_target = fs::read_link(path).ok().map(|p| p.display().to_string());
            kind = match fs::metadata(path) {
                Ok(t) if t.is_dir() => Kind::LinkDir,
                _ => Kind::Link,
            };
        } else if ft.is_dir() {
            kind = Kind::Dir;
        } else if ft.is_file() {
            kind = Kind::File;
        }
    }
    // Counting runs on the UI thread: cap it so node_modules-sized folders stay instant.
    let children = if kind.is_dir() { fs::read_dir(path).ok().map(|rd| rd.take(CHILD_CAP).count()) } else { None };
    Entry { id: 0, path: path.to_path_buf(), name, kind, size, mtime, mode, link_target, children, uid, gid }
}

fn ext_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => "",
    }
}

pub fn sort_entries(v: &mut [Entry], sort: Sort, reverse: bool) {
    v.sort_by(|a, b| {
        let o = match sort {
            Sort::Name => std::cmp::Ordering::Equal,
            Sort::Mtime => b.mtime.cmp(&a.mtime),
            Sort::Size => b.size.cmp(&a.size),
            Sort::Ext => ext_of(&a.name).to_lowercase().cmp(&ext_of(&b.name).to_lowercase()),
        }
        .then_with(|| natural_cmp(&a.name, &b.name));
        b.kind.is_dir().cmp(&a.kind.is_dir()).then(if reverse { o.reverse() } else { o })
    });
}

/// Case-insensitive natural order: file2 < file10.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(|c| c.is_ascii_digit()) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(|c| c.is_ascii_digit()) {
                    nb.push(c);
                    bi.next();
                }
                let o = na
                    .trim_start_matches('0')
                    .len()
                    .cmp(&nb.trim_start_matches('0').len())
                    .then_with(|| na.trim_start_matches('0').cmp(nb.trim_start_matches('0')));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let o = x.to_lowercase().cmp(y.to_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

pub fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut f = n as f64;
    let mut i = 0;
    while f >= 1024.0 && i < U.len() - 1 {
        f /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n}B")
    } else if f < 10.0 {
        format!("{f:.1}{}", U[i])
    } else {
        format!("{:.0}{}", f, U[i])
    }
}

/// Size column: human size for files, item count for directories.
pub fn size_label(e: &Entry) -> String {
    match (e.kind.is_dir(), e.children) {
        (true, Some(1)) => "1 item".into(),
        (true, Some(n)) if n >= CHILD_CAP => "1k+ items".into(),
        (true, Some(n)) => format!("{n} items"),
        (true, None) => String::new(),
        _ => human_size(e.size),
    }
}

pub fn age(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return String::new() };
    let Ok(d) = SystemTime::now().duration_since(t) else {
        return "now".into();
    };
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        86_400..=2_591_999 => format!("{}d", s / 86_400),
        2_592_000..=31_535_999 => format!("{}mo", s / 2_592_000),
        _ => format!("{}y", s / 31_536_000),
    }
}

/// User and group names for the metadata column (ids when unknown).
pub fn owner(uid: u32, gid: u32) -> String {
    use std::sync::{Mutex, OnceLock};
    static NAMES: OnceLock<Mutex<HashMap<(bool, u32), String>>> = OnceLock::new();
    let names = NAMES.get_or_init(Default::default);
    let lookup = |group: bool, id: u32| -> String {
        if let Some(n) = names.lock().ok().and_then(|m| m.get(&(group, id)).cloned()) {
            return n;
        }
        let mut buf = vec![0 as libc::c_char; 4096];
        let name = unsafe {
            if group {
                let mut g: libc::group = std::mem::zeroed();
                let mut out: *mut libc::group = std::ptr::null_mut();
                (libc::getgrgid_r(id, &mut g, buf.as_mut_ptr(), buf.len(), &mut out) == 0 && !out.is_null()).then(|| std::ffi::CStr::from_ptr(g.gr_name).to_string_lossy().into_owned())
            } else {
                let mut p: libc::passwd = std::mem::zeroed();
                let mut out: *mut libc::passwd = std::ptr::null_mut();
                (libc::getpwuid_r(id, &mut p, buf.as_mut_ptr(), buf.len(), &mut out) == 0 && !out.is_null()).then(|| std::ffi::CStr::from_ptr(p.pw_name).to_string_lossy().into_owned())
            }
        }
        .unwrap_or_else(|| id.to_string());
        if let Ok(mut m) = names.lock() {
            m.insert((group, id), name.clone());
        }
        name
    };
    format!("{}:{}", lookup(false, uid), lookup(true, gid))
}

/// The entry carries extended attributes (not following symlinks).
pub fn has_xattrs(p: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return false };
    #[cfg(target_os = "macos")]
    let n = unsafe { libc::listxattr(c.as_ptr(), std::ptr::null_mut(), 0, libc::XATTR_NOFOLLOW) };
    #[cfg(not(target_os = "macos"))]
    let n = unsafe { libc::llistxattr(c.as_ptr(), std::ptr::null_mut(), 0) };
    n > 0
}

/// A symlink whose target is missing.
pub fn broken_link(e: &Entry) -> bool {
    e.kind == Kind::Link && fs::metadata(&e.path).is_err()
}

pub fn perms(mode: u32, kind: Kind) -> String {
    let mut s = String::with_capacity(10);
    s.push(match kind {
        Kind::Dir => 'd',
        Kind::Link | Kind::LinkDir => 'l',
        _ => '-',
    });
    for shift in [6, 3, 0] {
        let b = (mode >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    s
}

/// Nerd-font icon + a colour key for a file name.
pub fn icon(name: &str, kind: Kind) -> (&'static str, IconColor) {
    use IconColor::*;
    match kind {
        Kind::Dir => {
            return match name {
                ".git" => ("\u{e5fb}", Peach),
                ".github" => ("\u{e5fd}", Dim),
                "node_modules" => ("\u{e5fa}", Green),
                "src" => ("\u{f08de}", Accent),
                _ => ("\u{f024b}", Accent),
            }
        }
        Kind::LinkDir => return ("\u{f1a5}", Teal),
        Kind::Link => return ("\u{f0337}", Teal),
        Kind::Other => return ("\u{f0214}", Dim),
        Kind::File => {}
    }
    let lower = name.to_lowercase();
    match lower.as_str() {
        "cargo.toml" | "cargo.lock" => return ("\u{e7a8}", Peach),
        "makefile" | "justfile" => return ("\u{e779}", Dim),
        "dockerfile" => return ("\u{f0868}", Blue),
        "license" | "license.md" => return ("\u{f0219}", Yellow),
        "readme.md" | "readme" => return ("\u{f00ba}", Yellow),
        ".gitignore" | ".gitattributes" | ".gitmodules" => return ("\u{e702}", Peach),
        _ => {}
    }
    match ext_of(&lower) {
        "rs" => ("\u{e7a8}", Peach),
        "py" => ("\u{e606}", Yellow),
        "js" | "mjs" | "cjs" => ("\u{e74e}", Yellow),
        "ts" | "mts" => ("\u{e628}", Blue),
        "tsx" | "jsx" => ("\u{e7ba}", Teal),
        "swift" => ("\u{e755}", Peach),
        "go" => ("\u{e627}", Teal),
        "c" | "h" => ("\u{e61e}", Blue),
        "cpp" | "cc" | "hpp" => ("\u{e61d}", Blue),
        "java" | "kt" => ("\u{e738}", Red),
        "rb" => ("\u{e739}", Red),
        "lua" => ("\u{e620}", Blue),
        "sh" | "bash" | "zsh" | "fish" => ("\u{f489}", Green),
        "md" | "markdown" | "mdx" => ("\u{f048a}", Text),
        "txt" | "log" => ("\u{f0219}", Dim),
        "json" | "jsonc" => ("\u{e60b}", Yellow),
        "toml" | "yaml" | "yml" | "ini" | "conf" | "cfg" => ("\u{e615}", Dim),
        "html" | "htm" => ("\u{e736}", Peach),
        "css" | "scss" => ("\u{e749}", Blue),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "heic" => ("\u{f021f}", Teal),
        "pdf" => ("\u{f0226}", Red),
        "zip" | "gz" | "tgz" | "xz" | "bz2" | "7z" | "tar" | "zst" => ("\u{f0187}", Peach),
        "mp3" | "wav" | "flac" | "m4a" => ("\u{f0388}", Teal),
        "mp4" | "mov" | "mkv" | "webm" => ("\u{f0381}", Teal),
        "doc" | "docx" | "odt" => ("\u{f0219}", Blue),
        "xls" | "xlsx" | "csv" | "tsv" => ("\u{f021b}", Green),
        "lock" => ("\u{f033e}", Dim),
        "vim" => ("\u{e62b}", Green),
        "nix" => ("\u{f1105}", Blue),
        _ => ("\u{f0214}", Text),
    }
}

#[derive(Clone, Copy, Debug)]
pub enum IconColor {
    Accent,
    Text,
    Dim,
    Peach,
    Green,
    Yellow,
    Blue,
    Teal,
    Red,
}

/// Git status of an entry: the one-letter summary plus whether the change
/// sits in the index (staged), in the work tree (unstaged), or both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Git {
    pub letter: char,
    pub staged: bool,
    pub unstaged: bool,
}

impl Git {
    pub fn from_xy(xy: &str) -> Git {
        let mut c = xy.chars();
        let (x, y) = (c.next().unwrap_or(' '), c.next().unwrap_or(' '));
        let real = |c: char| !matches!(c, ' ' | '?' | '!');
        let conflict = xy.contains('U') || xy == "AA" || xy == "DD";
        Git { letter: status_letter(xy), staged: real(x) && !conflict, unstaged: real(y) && !conflict }
    }

    fn rank(self) -> u8 {
        match self.letter {
            'U' => 6,
            'M' | 'R' | 'D' => 5,
            'A' => 4,
            '?' => 3,
            '!' => 1,
            _ => 0,
        }
    }

    /// Fold a child's status into a directory's: the strongest letter wins,
    /// staged / unstaged accumulate.
    pub fn merge(&mut self, o: Git) {
        if o.rank() > self.rank() {
            self.letter = o.letter;
        }
        self.staged |= o.staged;
        self.unstaged |= o.unstaged;
    }

    /// Both staged and unstaged changes (`MM`, `AM`, or a folder holding both).
    pub fn partial(self) -> bool {
        self.staged && self.unstaged
    }
}

/// Branch line of `git status -b`: `main...origin/main [ahead 1, behind 2]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub detached: bool,
    /// No commits yet.
    pub unborn: bool,
}

impl Branch {
    /// Parse the text after `## `.
    pub fn parse(h: &str) -> Branch {
        let mut b = Branch::default();
        let h = h.trim();
        if let Some(n) = h.strip_prefix("No commits yet on ").or_else(|| h.strip_prefix("Initial commit on ")) {
            b.name = n.trim().to_string();
            b.unborn = true;
            return b;
        }
        if h.starts_with("HEAD (no branch)") {
            b.name = "HEAD".into();
            b.detached = true;
            return b;
        }
        let (head, track) = match h.split_once(" [") {
            Some((a, t)) => (a, Some(t.trim_end_matches(']'))),
            None => (h, None),
        };
        match head.split_once("...") {
            Some((n, u)) => {
                b.name = n.to_string();
                b.upstream = Some(u.to_string());
            }
            None => b.name = head.to_string(),
        }
        for part in track.into_iter().flat_map(|t| t.split(", ")) {
            if let Some(n) = part.strip_prefix("ahead ") {
                b.ahead = n.parse().unwrap_or(0);
            } else if let Some(n) = part.strip_prefix("behind ") {
                b.behind = n.parse().unwrap_or(0);
            }
        }
        b
    }

    /// Compact badge: ` main ↑1↓2`.
    pub fn badge(&self) -> String {
        let mut s = self.name.clone();
        if self.unborn {
            s.push_str(" (no commits)");
        }
        if self.ahead > 0 {
            s.push_str(&format!(" ↑{}", self.ahead));
        }
        if self.behind > 0 {
            s.push_str(&format!(" ↓{}", self.behind));
        }
        s
    }
}

/// The work tree containing `dir`: the nearest ancestor with a `.git`
/// directory or file (worktrees, submodules), found without running git.
pub fn work_tree(dir: &Path) -> Option<PathBuf> {
    let dir = fs::canonicalize(dir).ok()?;
    dir.ancestors().find(|d| fs::symlink_metadata(d.join(".git")).is_ok()).map(Path::to_path_buf)
}

/// How long a "no filters here" verdict is trusted without re-checking
/// (config can include other files whose changes we cannot see).
const FILTER_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// Modification times of the repository's own config files: a change there
/// invalidates the cached filter verdict at once.
fn config_stamp(root: &Path) -> Vec<Option<SystemTime>> {
    let git = root.join(".git");
    let gitdir = if git.is_dir() { git } else { root.to_path_buf() };
    ["config", "config.worktree"].iter().map(|f| fs::metadata(gitdir.join(f)).and_then(|m| m.modified()).ok()).collect()
}

type FilterCache = HashMap<PathBuf, (std::time::Instant, Vec<Option<SystemTime>>, bool)>;

fn filter_cache() -> &'static std::sync::Mutex<FilterCache> {
    static C: std::sync::OnceLock<std::sync::Mutex<FilterCache>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// True when any config git would read for `dir` defines a clean/smudge/
/// process filter outside the user's own global/system files. `git status`
/// may run clean filters, so a repository that carries its own config (an
/// unpacked archive, a shared folder; local, worktree or included files)
/// could make us execute arbitrary commands. Reading config never runs filters.
///
/// The verdict is cached per work tree (keyed by its config files' mtimes,
/// and for [`FILTER_TTL`] at most) so walking around a repository costs no
/// extra `git config` call per directory.
pub fn repo_defines_filters(dir: &Path) -> bool {
    let root = work_tree(dir);
    if let Some(r) = &root {
        let stamp = config_stamp(r);
        if let Some((t, st, v)) = filter_cache().lock().ok().and_then(|c| c.get(r).cloned()) {
            if st == stamp && t.elapsed() < FILTER_TTL {
                return v;
            }
        }
        let v = filters_uncached(dir);
        if let Ok(mut c) = filter_cache().lock() {
            c.insert(r.clone(), (std::time::Instant::now(), stamp, v));
        }
        return v;
    }
    filters_uncached(dir)
}

fn filters_uncached(dir: &Path) -> bool {
    let out = std::process::Command::new("git")
        .args(["config", "--list", "--show-scope", "--includes", "--name-only"])
        .current_dir(dir)
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(o) = out else { return true };
    if !o.status.success() {
        // not a repository (status would fail anyway) or unreadable config
        return !o.stdout.is_empty();
    }
    String::from_utf8_lossy(&o.stdout).lines().any(|l| {
        let (scope, key) = l.split_once('\t').unwrap_or(("", l));
        let key = key.to_ascii_lowercase();
        let filter = key.starts_with("filter.") && (key.ends_with(".clean") || key.ends_with(".smudge") || key.ends_with(".process"));
        filter && !matches!(scope, "global" | "system" | "command")
    })
}

/// A `git status` record: porcelain `XY`, path relative to the listed
/// directory, and the source of a rename / copy.
pub struct Record {
    pub xy: String,
    pub path: String,
    pub orig: Option<String>,
}

pub struct Status {
    pub records: Vec<Record>,
    pub branch: Option<Branch>,
}

/// `git status --porcelain=v1 -b -z` for `dir` (paths relative to it), run
/// with every hook/filter/external-diff knob off. `None` outside a repo or
/// when the repository carries its own filters. One subprocess: the work
/// tree is found in Rust and the filter check is cached.
fn status_records(dir: &Path, untracked: &str, ignored: bool) -> Option<Status> {
    let root = work_tree(dir)?;
    if repo_defines_filters(dir) {
        return None;
    }
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "diff.external=",
            "-c",
            "status.submoduleSummary=false",
            "-c",
            "status.renames=true",
            "--no-optional-locks",
            "status",
            "--ignore-submodules=all",
            "--porcelain=v1",
            "-b",
            "-z",
            if ignored { "--ignored=matching" } else { "--ignored=no" },
            untracked,
            "--",
            ".",
        ])
        .current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // Paths are relative to the repo root; our prefix inside it, in Rust
    // (both sides canonical, or /tmp vs /private/tmp would drop everything).
    let canon = fs::canonicalize(dir).ok()?;
    let top = match canon.strip_prefix(&root) {
        Ok(p) if p.as_os_str().is_empty() => String::new(),
        Ok(p) => format!("{}/", p.to_string_lossy()),
        Err(_) => return None,
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut recs = Vec::new();
    let mut branch = None;
    let mut it = text.split('\0');
    while let Some(rec) = it.next() {
        if let Some(h) = rec.strip_prefix("## ") {
            branch = Some(Branch::parse(h));
            continue;
        }
        if rec.len() < 4 {
            continue;
        }
        let (xy, path) = rec.split_at(3);
        let xy = &xy[..2];
        let orig = if xy.starts_with('R') || xy.starts_with('C') { it.next() } else { None };
        let orig = orig.and_then(|o| o.strip_prefix(top.as_str())).map(str::to_string);
        let Some(rel) = path.strip_prefix(top.as_str()) else {
            // moved here from outside this folder: the source is elsewhere
            continue;
        };
        recs.push(Record { xy: xy.to_string(), path: rel.to_string(), orig });
    }
    Some(Status { records: recs, branch })
}

/// One letter for a porcelain XY code.
fn status_letter(xy: &str) -> char {
    match xy {
        "!!" => '!',
        "??" => '?',
        _ if xy.contains('U') || xy == "AA" || xy == "DD" => 'U',
        _ if xy.contains('R') => 'R',
        _ if xy.contains('A') => 'A',
        _ if xy.contains('D') => 'D',
        _ => 'M',
    }
}

/// Changed and untracked files under `dir` (relative paths), for the
/// finder's Git tab. `None` when `dir` is not (safely) in a repository.
pub fn git_changes(dir: &Path) -> Option<Vec<(Git, String)>> {
    let st = status_records(dir, "--untracked-files=all", false)?;
    Some(st.records.into_iter().map(|r| (Git::from_xy(&r.xy), r.path)).collect())
}

/// Git status per direct child of `dir` (folders aggregate their contents;
/// the folder a file was renamed out of counts as changed too), plus the
/// branch. Empty outside a repository or when git is unavailable.
pub fn git_status_full(dir: &Path) -> (HashMap<String, Git>, Option<Branch>) {
    let mut map: HashMap<String, Git> = HashMap::new();
    let Some(st) = status_records(dir, "--untracked-files=normal", true) else { return (map, None) };
    for r in st.records {
        let g = Git::from_xy(&r.xy);
        let paths = std::iter::once(r.path.as_str()).chain(r.orig.as_deref());
        for (k, p) in paths.enumerate() {
            let mut parts = p.split('/');
            let child = parts.next().unwrap_or("").trim_end_matches('/');
            // the source of a rename only marks a folder it left
            if child.is_empty() || (k == 1 && !parts.next().is_some_and(|s| !s.is_empty())) {
                continue;
            }
            map.entry(child.to_string()).and_modify(|s| s.merge(g)).or_insert(g);
        }
    }
    (map, st.branch)
}

pub fn git_status(dir: &Path) -> HashMap<String, Git> {
    git_status_full(dir).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["file10", "File2", "file1", "a"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a", "file1", "File2", "file10"]);
    }

    #[test]
    fn reverse_keeps_dirs_first() {
        let e = |name: &str, kind| Entry { id: 0, path: PathBuf::from(name), name: name.into(), kind, size: 0, mtime: None, mode: 0, link_target: None, children: None, uid: 0, gid: 0 };
        let mut v = vec![e("b", Kind::File), e("z", Kind::Dir), e("a", Kind::File), e("c", Kind::Dir)];
        sort_entries(&mut v, Sort::Name, true);
        assert_eq!(v.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["z", "c", "b", "a"]);
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(1536), "1.5K");
        assert_eq!(human_size(50 * 1024 * 1024), "50M");
    }

    #[test]
    fn skips_repos_with_local_filters() {
        let t = std::env::temp_dir().join(format!("herdr-oil-git-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        fs::create_dir_all(&t).unwrap();
        let git = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&t).output().unwrap();
        git(&["init", "-q"]);
        fs::write(t.join("a.txt"), "x").unwrap();
        assert_eq!(git_status(&t).get("a.txt").map(|g| g.letter), Some('?'));
        let marker = t.join("pwned");
        git(&["config", "filter.evil.clean", &format!("touch {}", marker.display())]);
        fs::write(t.join(".gitattributes"), "* filter=evil\n").unwrap();
        assert!(git_status(&t).is_empty());
        assert!(!marker.exists());
        // worktree-scoped config is just as untrusted
        git(&["config", "--unset", "filter.evil.clean"]);
        assert_eq!(git_status(&t).get("a.txt").map(|g| g.letter), Some('?'));
        git(&["config", "extensions.worktreeConfig", "true"]);
        git(&["config", "--worktree", "filter.evil.process", &format!("touch {}", marker.display())]);
        assert!(git_status(&t).is_empty());
        assert!(!marker.exists());
        let _ = fs::remove_dir_all(&t);
    }

    #[test]
    fn staged_unstaged_and_renames() {
        let t = crate::plan::tests::tmpdir("git-xy");
        let git = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&t.0).output().unwrap();
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        fs::create_dir(t.0.join("sub")).unwrap();
        fs::write(t.0.join("a.txt"), "a\n").unwrap();
        fs::write(t.0.join("b.txt"), "b\n").unwrap();
        fs::write(t.0.join("sub/c.txt"), "c\n").unwrap();
        fs::write(t.0.join("sub/d.txt"), "d\n").unwrap();
        let (_, br) = git_status_full(&t.0);
        assert!(br.as_ref().is_some_and(|b| b.unborn && b.name == "main"), "{br:?}");
        git(&["add", "."]);
        git(&["commit", "-qm", "x"]);
        fs::write(t.0.join("a.txt"), "a2\n").unwrap();
        git(&["add", "a.txt"]);
        fs::write(t.0.join("a.txt"), "a3\n").unwrap();
        fs::write(t.0.join("b.txt"), "b2\n").unwrap();
        git(&["mv", "sub/c.txt", "c.txt"]);
        let (m, br) = git_status_full(&t.0);
        assert_eq!(br.map(|b| b.badge()), Some("main".to_string()));
        let a = m["a.txt"];
        assert!(a.partial() && a.letter == 'M');
        let b = m["b.txt"];
        assert!(!b.staged && b.unstaged);
        assert_eq!(m["c.txt"].letter, 'R');
        assert!(m["c.txt"].staged);
        assert!(m.get("sub").is_some_and(|g| g.letter == 'R'), "folder a file was moved out of");
        let sub = git_status(&t.0.join("sub"));
        // seen from sub/ alone, the moved-away file reads as deleted
        assert!(!sub.contains_key("d.txt") && sub.get("c.txt").is_some_and(|g| g.letter == 'D'), "{sub:?}");
    }

    #[test]
    fn branch_headers() {
        let b = Branch::parse("main...origin/main [ahead 2, behind 1]");
        assert_eq!((b.name.as_str(), b.upstream.as_deref(), b.ahead, b.behind), ("main", Some("origin/main"), 2, 1));
        assert_eq!(b.badge(), "main ↑2 ↓1");
        assert!(Branch::parse("HEAD (no branch)").detached);
        assert_eq!(Branch::parse("feature").upstream, None);
        assert!(Branch::parse("No commits yet on dev").unborn);
    }

    #[test]
    fn metadata_helpers() {
        let t = crate::plan::tests::tmpdir("meta");
        std::os::unix::fs::symlink(t.0.join("missing"), t.0.join("dead")).unwrap();
        let e = stat_entry(&t.0.join("dead"), "dead".into());
        assert!(broken_link(&e));
        let me = stat_entry(&t.0, "t".into());
        assert!(owner(me.uid, me.gid).contains(':'));
        let _ = has_xattrs(&t.0.join("dead"));
        assert!(!has_xattrs(&t.0.join("not-there")));
    }

    #[test]
    fn perm_string() {
        assert_eq!(perms(0o755, Kind::Dir), "drwxr-xr-x");
        assert_eq!(perms(0o644, Kind::File), "-rw-r--r--");
    }
}
