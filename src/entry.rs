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
}

impl Entry {
    /// Text shown (and edited) in the buffer: dirs get a trailing slash.
    pub fn display(&self) -> String {
        if self.kind.is_dir() {
            format!("{}/", self.name)
        } else {
            self.name.clone()
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
        sort_entries(&mut items, sort);
        Ok(items.into_iter().map(|e| self.intern(e)).collect())
    }
}

pub fn stat_entry(path: &Path, name: String) -> Entry {
    let lmeta = fs::symlink_metadata(path).ok();
    let mut kind = Kind::Other;
    let mut link_target = None;
    let mut size = 0;
    let mut mtime = None;
    let mut mode = 0;
    if let Some(m) = &lmeta {
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
    let children = if kind.is_dir() { fs::read_dir(path).ok().map(|rd| rd.take(100_000).count()) } else { None };
    Entry { id: 0, path: path.to_path_buf(), name, kind, size, mtime, mode, link_target, children }
}

fn ext_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => "",
    }
}

pub fn sort_entries(v: &mut [Entry], sort: Sort) {
    v.sort_by(|a, b| {
        b.kind
            .is_dir()
            .cmp(&a.kind.is_dir())
            .then_with(|| match sort {
                Sort::Name => std::cmp::Ordering::Equal,
                Sort::Mtime => b.mtime.cmp(&a.mtime),
                Sort::Size => b.size.cmp(&a.size),
                Sort::Ext => ext_of(&a.name).to_lowercase().cmp(&ext_of(&b.name).to_lowercase()),
            })
            .then_with(|| natural_cmp(&a.name, &b.name))
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
        (true, Some(n)) if n >= 100_000 => "100k+ items".into(),
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

/// One-letter git status per direct child of `dir` (dirs aggregate their
/// contents). Empty map when not in a repo or git is unavailable.
pub fn git_status(dir: &Path) -> HashMap<String, char> {
    let mut map = HashMap::new();
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
            "--no-optional-locks",
            "status",
            "--ignore-submodules=all",
            "--no-renames",
            "--porcelain=v1",
            "-z",
            "--ignored=matching",
            "--untracked-files=normal",
            "--",
            ".",
        ])
        .current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(out) = out else { return map };
    if !out.status.success() {
        return map;
    }
    // Paths are relative to the repo root; find our prefix inside it.
    let top = std::process::Command::new("git")
        .args(["rev-parse", "--show-prefix"])
        .current_dir(dir)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&out.stdout);
    let mut it = text.split('\0');
    while let Some(rec) = it.next() {
        if rec.len() < 4 {
            continue;
        }
        let (xy, path) = rec.split_at(3);
        let xy = &xy[..2];
        if xy.starts_with('R') || xy.starts_with('C') {
            it.next(); // rename source
        }
        let Some(rel) = path.strip_prefix(top.as_str()) else { continue };
        let child = rel.split('/').next().unwrap_or("").trim_end_matches('/');
        if child.is_empty() {
            continue;
        }
        let c = match xy {
            "!!" => '!',
            "??" => '?',
            _ if xy.contains('U') => 'U',
            _ if xy.contains('A') => 'A',
            _ if xy.contains('D') => 'D',
            _ if xy.contains('R') => 'R',
            _ => 'M',
        };
        let rank = |c: char| match c {
            'U' => 6,
            'M' | 'R' | 'D' => 5,
            'A' => 4,
            '?' => 3,
            '!' => 1,
            _ => 0,
        };
        let slot = map.entry(child.to_string()).or_insert(c);
        if rank(c) > rank(*slot) {
            *slot = c;
        }
    }
    map
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
    fn sizes() {
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(1536), "1.5K");
        assert_eq!(human_size(50 * 1024 * 1024), "50M");
    }

    #[test]
    fn perm_string() {
        assert_eq!(perms(0o755, Kind::Dir), "drwxr-xr-x");
        assert_eq!(perms(0o644, Kind::File), "-rw-r--r--");
    }
}
