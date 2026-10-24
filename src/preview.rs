//! Preview pane content: syntax-highlighted text through `bat` when it is
//! installed (ANSI parsed into ratatui spans), plain numbered text
//! otherwise, a listing with stats and README for directories, contents of
//! archives / SQLite schemas / binary plists, and a summary for other binaries.

use crate::entry::{self, Kind, Sort, Store};
use crate::theme::Theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// What a preview shows for a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    File,
    /// The structured `git diff` (the file itself when unchanged).
    Diff,
    /// `git log --graph` of the path.
    Log,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Key {
    pub path: PathBuf,
    pub line: Option<usize>,
    pub width: u16,
    pub max: usize,
    pub mtime: Option<std::time::SystemTime>,
    pub show: Show,
    pub hidden: bool,
}

type Rendered = (Key, Vec<Line<'static>>, usize);

/// Previews are rendered on a worker thread (bat, git, tar, unzip can take
/// tens of milliseconds): the draw asks for a key and keeps showing what it
/// has until the worker answers; stale requests are skipped.
#[derive(Default)]
pub struct Cache {
    pub key: Option<Key>,
    pub lines: Vec<Line<'static>>,
    /// Index into `lines` to scroll to (grep hit).
    pub focus: usize,
    /// Requested, not rendered yet.
    pub pending: Option<Key>,
    tx: Option<std::sync::mpsc::Sender<(Key, Theme)>>,
    rx: Option<std::sync::mpsc::Receiver<Rendered>>,
}


pub fn bat_bin() -> Option<&'static str> {
    static BAT: OnceLock<Option<&'static str>> = OnceLock::new();
    *BAT.get_or_init(|| {
        for b in ["bat", "batcat"] {
            let ok = Command::new(b)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                return Some(b);
            }
        }
        None
    })
}

/// Line cap for the side preview; the full viewer asks for far more.
pub const PREVIEW_LINES: usize = 400;
pub const VIEWER_LINES: usize = 50_000;

impl Cache {
    fn worker(&mut self) -> Option<&std::sync::mpsc::Sender<(Key, Theme)>> {
        if self.tx.is_none() {
            let (tx, req) = std::sync::mpsc::channel::<(Key, Theme)>();
            let (done, rx) = std::sync::mpsc::channel::<Rendered>();
            std::thread::spawn(move || {
                while let Ok(mut job) = req.recv() {
                    // only the newest request matters
                    while let Ok(newer) = req.try_recv() {
                        job = newer;
                    }
                    let (key, th) = job;
                    let (lines, focus) = render_key(&key, &th);
                    if done.send((key, lines, focus)).is_err() {
                        break;
                    }
                }
            });
            self.tx = Some(tx);
            self.rx = Some(rx);
        }
        self.tx.as_ref()
    }

    /// Ask for the preview of `path`; returns the cache with the newest
    /// rendering available (check [`Cache::ready`]).
    #[allow(clippy::too_many_arguments)]
    pub fn get(&mut self, path: &Path, line: Option<usize>, width: u16, th: &Theme, hidden: bool, max: usize, show: Show) -> &Cache {
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let key = Key { path: path.to_path_buf(), line, width, max, mtime, show, hidden };
        self.poll();
        if self.key.as_ref() != Some(&key) && self.pending.as_ref() != Some(&key) {
            let sent = self.worker().is_some_and(|tx| tx.send((key.clone(), th.clone())).is_ok());
            if sent {
                self.pending = Some(key);
            } else {
                let (lines, focus) = render_key(&key, th);
                (self.lines, self.focus, self.key) = (lines, focus, Some(key));
            }
        }
        self
    }

    /// Take finished renderings; true when the shown preview changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        let mut changed = false;
        while let Ok((key, lines, focus)) = rx.try_recv() {
            if self.pending.as_ref() == Some(&key) {
                self.pending = None;
            }
            (self.lines, self.focus, self.key) = (lines, focus, Some(key));
            changed = true;
        }
        changed
    }

    /// The shown lines belong to the last requested key.
    pub fn ready(&self) -> bool {
        self.pending.is_none() && self.key.is_some()
    }

    /// Forget the rendering (the file changed under it).
    pub fn invalidate(&mut self) {
        self.key = None;
    }

    /// Render synchronously.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn get_now(&mut self, path: &Path, line: Option<usize>, width: u16, th: &Theme, hidden: bool, max: usize, show: Show) -> &Cache {
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let key = Key { path: path.to_path_buf(), line, width, max, mtime, show, hidden };
        let (lines, focus) = render_key(&key, th);
        (self.lines, self.focus, self.key, self.pending) = (lines, focus, Some(key), None);
        self
    }
}

fn render_key(k: &Key, th: &Theme) -> (Vec<Line<'static>>, usize) {
    match k.show {
        Show::Diff => git_diff(&k.path, th, k.width, k.max).map(|v| (v, 0)).unwrap_or_else(|| render(&k.path, k.line, k.width, th, k.hidden, k.max)),
        Show::Log => (git_log(&k.path, th, k.max), 0),
        Show::File => render(&k.path, k.line, k.width, th, k.hidden, k.max),
    }
}

/// `git log --graph` of a path, coloured by git.
fn git_log(path: &Path, th: &Theme, max: usize) -> Vec<Line<'static>> {
    match crate::git::log(path, max.min(200)) {
        Ok(v) if v.is_empty() => vec![Line::styled(" no commits touch this path yet", Style::default().fg(th.dim))],
        Ok(v) => {
            let mut out = vec![Line::styled(" git log", Style::default().fg(th.subtext).add_modifier(Modifier::BOLD)), Line::raw("")];
            out.extend(v.iter().map(|l| parse_ansi(&format!(" {l}"))));
            out
        }
        Err(e) => vec![Line::styled(format!(" {e}"), Style::default().fg(th.red))],
    }
}

fn render(path: &Path, line: Option<usize>, width: u16, th: &Theme, hidden: bool, max: usize) -> (Vec<Line<'static>>, usize) {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return (vec![Line::styled(format!(" {e}"), Style::default().fg(th.red))], 0),
    };
    if !meta.is_dir() && !meta.is_file() {
        // FIFOs, sockets and devices: opening them can block forever.
        return (vec![Line::styled(" special file — no preview", Style::default().fg(th.dim))], 0);
    }
    if meta.is_dir() {
        return (dir_listing(path, th, hidden, max, width), 0);
    }
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
        return (pdf(path, th, max, meta.len()), 0);
    }
    // binary sniff
    let mut head = [0u8; 8192];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    if head[..n].contains(&0) {
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if let Some(v) = special(path, &head[..n], th, max) {
            return (v, 0);
        }
        let mut v = vec![
            Line::styled(" binary file", Style::default().fg(th.subtext).add_modifier(Modifier::BOLD)),
            Line::styled(format!(" {}  ·  {ext}", entry::human_size(meta.len())), Style::default().fg(th.dim)),
        ];
        if let Some(out) = run_capped(Command::new("file").arg("-b").arg(path), 4) {
            v.push(Line::raw(""));
            v.extend(out.into_iter().map(|s| Line::styled(format!(" {s}"), Style::default().fg(th.dim))));
        }
        if cfg!(target_os = "macos") && is_image(&ext) {
            if let Some(out) = run_capped(Command::new("sips").args(["-g", "pixelWidth", "-g", "pixelHeight", "-g", "format", "-g", "space"]).arg(path), 8) {
                v.push(Line::raw(""));
                // first line repeats the path
                v.extend(out.into_iter().skip(1).map(|s| Line::styled(format!(" {}", s.trim()), Style::default().fg(th.subtext))));
            }
        }
        return (v, 0);
    }
    let (start, end) = match line {
        Some(l) => (l.saturating_sub(max / 4).max(1), l + max * 3 / 4),
        None => (1, max),
    };
    if let Some(bat) = bat_bin() {
        let mut cmd = Command::new(bat);
        cmd.args(["--color=always", "--style=numbers", "--paging=never", "--wrap=never"])
            .arg(format!("--terminal-width={}", width.max(20)))
            .arg(format!("--line-range={start}:{end}"));
        if let Some(l) = line {
            cmd.arg(format!("--highlight-line={l}"));
        }
        cmd.arg("--").arg(path).stdin(Stdio::null()).stderr(Stdio::null());
        if let Ok(o) = cmd.output() {
            if o.status.success() && !o.stdout.is_empty() {
                let text = String::from_utf8_lossy(&o.stdout);
                let lines: Vec<Line<'static>> = text.lines().map(parse_ansi).collect();
                let focus = line.map(|l| l - start).unwrap_or(0);
                return (lines, focus);
            }
        }
    }
    // plain fallback
    let text = std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let mut v = Vec::new();
    for (i, l) in text.lines().enumerate().skip(start - 1).take(end - start + 1) {
        let n = i + 1;
        let style = if Some(n) == line { Style::default().bg(th.surface) } else { Style::default() };
        v.push(Line::from(vec![
            Span::styled(format!("{n:>4} "), Style::default().fg(th.dim)),
            Span::styled(l.replace('\t', "    "), style.fg(th.text)),
        ]));
    }
    if v.is_empty() {
        v.push(Line::styled(" (empty file)", Style::default().fg(th.dim)));
    }
    (v, line.map(|l| l - start).unwrap_or(0))
}

/// Structured `git diff` of a file (HEAD → work tree; deleted and
/// untracked files too), rendered compactly. `None` when it has no changes
/// or git cannot (safely) run there.
fn git_diff(path: &Path, th: &Theme, width: u16, max: usize) -> Option<Vec<Line<'static>>> {
    let o = crate::diff::Opts::default();
    let mut files = crate::diff::load(path, false, o).ok()?;
    if files.is_empty() && path.is_file() && crate::entry::git_status(path.parent()?).get(&path.file_name()?.to_string_lossy().into_owned()).is_some_and(|g| g.letter == '?') {
        // not tracked yet: all of it is new
        files = crate::diff::load(path, true, o).ok()?;
    }
    if files.is_empty() {
        return None;
    }
    Some(crate::diff::render_compact(&files, th, width, max))
}

/// PDF: `pdfinfo` metadata and the first pages' text via `pdftotext`
/// (poppler), else Spotlight's `mdls` on macOS, else just the header.
fn pdf(path: &Path, th: &Theme, max: usize, size: u64) -> Vec<Line<'static>> {
    let mut v = vec![Line::from(vec![
        Span::styled(" PDF document", Style::default().fg(th.subtext).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  ·  {}", entry::human_size(size)), Style::default().fg(th.dim)),
    ])];
    let info = run_capped(Command::new("pdfinfo").arg(path), 30).or_else(|| {
        cfg!(target_os = "macos")
            .then(|| run_capped(Command::new("mdls").args(["-name", "kMDItemTitle", "-name", "kMDItemAuthors", "-name", "kMDItemNumberOfPages", "-name", "kMDItemPageWidth", "-name", "kMDItemPageHeight", "-name", "kMDItemCreator"]).arg(path), 30))
            .flatten()
    });
    if let Some(info) = info {
        v.push(Line::raw(""));
        for l in info.into_iter().filter(|l| !l.trim_end().ends_with("(null)")) {
            let (k, val) = l.split_once(':').or_else(|| l.split_once('=')).unwrap_or((&l, ""));
            v.push(Line::from(vec![
                Span::styled(format!(" {:<16}", k.trim().trim_start_matches("kMDItem")), Style::default().fg(th.dim)),
                Span::styled(val.trim().to_string(), Style::default().fg(th.text)),
            ]));
        }
    }
    if let Some(text) = run_capped(Command::new("pdftotext").args(["-l", "3", "-layout", "-enc", "UTF-8"]).arg(path).arg("-"), max) {
        v.push(Line::raw(""));
        v.push(Line::styled(" text (first pages)", Style::default().fg(th.yellow).add_modifier(Modifier::BOLD)));
        v.extend(text.into_iter().map(|l| Line::styled(format!(" {}", l.chars().filter(|c| !c.is_control()).collect::<String>()), Style::default().fg(th.subtext))));
    } else if v.len() == 1 {
        v.push(Line::styled(" install poppler (pdfinfo, pdftotext) for metadata and text", Style::default().fg(th.dim)));
    }
    v
}

fn is_image(ext: &str) -> bool {
    matches!(ext, "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "tif" | "tiff" | "bmp" | "ico")
}

/// Run a helper and keep at most `max` lines of its output; the process is
/// killed after that so a huge archive cannot stall the preview.
fn run_capped(cmd: &mut Command, max: usize) -> Option<Vec<String>> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let out = child.stdout.take()?;
    let mut lines = Vec::new();
    let mut more = false;
    for l in BufReader::new(out).lines() {
        let Ok(l) = l else { break };
        if lines.len() >= max {
            more = true;
            break;
        }
        lines.push(l.replace('\t', "    "));
    }
    let _ = child.kill();
    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    if lines.is_empty() || (!ok && !more) {
        return None;
    }
    if more {
        lines.push("…".into());
    }
    Some(lines)
}

/// Archive listings, SQLite schemas and binary plists.
fn special(path: &Path, head: &[u8], th: &Theme, max: usize) -> Option<Vec<Line<'static>>> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    let has = |exts: &[&str]| exts.iter().any(|e| name.ends_with(e));
    let (title, out) = if has(&[".zip", ".jar", ".whl", ".apk", ".epub", ".nupkg"]) {
        ("zip archive", run_capped(Command::new("unzip").arg("-l").arg(path), max)?)
    } else if has(&[".tar", ".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.bz2", ".tbz2", ".tar.zst"]) {
        ("tar archive", run_capped(Command::new("tar").arg("-tvf").arg(path), max)?)
    } else if head.starts_with(b"SQLite format 3\0") {
        ("SQLite database · schema", run_capped(Command::new("sqlite3").arg("-readonly").arg(path).arg(".schema"), max)?)
    } else if head.starts_with(b"bplist") && cfg!(target_os = "macos") {
        ("binary plist", run_capped(Command::new("plutil").arg("-p").arg(path), max)?)
    } else {
        return None;
    };
    let size = std::fs::metadata(path).map(|m| entry::human_size(m.len())).unwrap_or_default();
    let mut v = vec![
        Line::from(vec![
            Span::styled(format!(" {title}"), Style::default().fg(th.subtext).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  ·  {size}"), Style::default().fg(th.dim)),
        ]),
        Line::raw(""),
    ];
    v.extend(out.into_iter().map(|l| Line::styled(format!(" {l}"), Style::default().fg(th.text))));
    Some(v)
}

fn dir_listing(path: &Path, th: &Theme, hidden: bool, max: usize, width: u16) -> Vec<Line<'static>> {
    let mut store = Store::default();
    let ids = match store.list(path, hidden, Sort::Name) {
        Ok(v) => v,
        Err(e) => return vec![Line::styled(format!(" {e}"), Style::default().fg(th.red))],
    };
    if ids.is_empty() {
        return vec![Line::styled(" (empty directory)", Style::default().fg(th.dim).add_modifier(Modifier::ITALIC))];
    }
    let entries: Vec<&entry::Entry> = ids.iter().filter_map(|id| store.get(*id)).collect();
    let dirs = entries.iter().filter(|e| e.kind.is_dir()).count();
    let files = entries.len() - dirs;
    let bytes: u64 = entries.iter().filter(|e| e.kind == Kind::File).map(|e| e.size).sum();
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    let mut stats = vec![];
    if dirs > 0 {
        stats.push(plural(dirs, "folder"));
    }
    if files > 0 {
        stats.push(format!("{} · {}", plural(files, "file"), entry::human_size(bytes)));
    }
    let mut v = vec![Line::styled(format!(" {}", stats.join(" · ")), Style::default().fg(th.dim)), Line::raw("")];
    let w = width.saturating_sub(1) as usize;
    for e in entries.iter().take(max) {
        let (ic, col) = entry::icon(&e.name, e.kind);
        let fg = match e.kind {
            Kind::Dir | Kind::LinkDir => th.accent,
            _ if e.is_exec() => th.green,
            _ => th.text,
        };
        let name = e.display();
        let size = entry::size_label(e);
        let used = 4 + unicode_width::UnicodeWidthStr::width(name.as_str());
        let mut spans = vec![
            Span::styled(format!(" {ic} "), Style::default().fg(crate::ui::icon_color(th, col))),
            Span::styled(name, Style::default().fg(fg).add_modifier(if e.kind.is_dir() { Modifier::BOLD } else { Modifier::empty() })),
        ];
        if w > used + size.len() + 2 && w > 30 {
            spans.push(Span::raw(" ".repeat(w - used - size.len())));
            spans.push(Span::styled(size, Style::default().fg(th.dim)));
        }
        v.push(Line::from(spans));
    }
    if entries.len() > max {
        v.push(Line::styled(format!(" … {} more", entries.len() - max), Style::default().fg(th.dim)));
    }
    // README snippet under the listing
    if let Some(r) = entries.iter().find(|e| e.kind == Kind::File && matches!(e.name.to_lowercase().as_str(), "readme.md" | "readme" | "readme.txt" | "readme.rst")) {
        if let Ok(text) = std::fs::read(&r.path).map(|b| String::from_utf8_lossy(&b[..b.len().min(16 * 1024)]).into_owned()) {
            v.push(Line::raw(""));
            let rule = "─".repeat(w.saturating_sub(r.name.len() + 4).min(60));
            v.push(Line::from(vec![
                Span::styled(format!(" {} ", r.name), Style::default().fg(th.yellow).add_modifier(Modifier::BOLD)),
                Span::styled(rule, Style::default().fg(th.dim)),
            ]));
            for l in text.lines().take(20) {
                let style = if l.starts_with('#') { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.subtext) };
                v.push(Line::styled(format!(" {}", l.replace('\t', "    ")), style));
            }
        }
    }
    v
}

fn ansi_color(n: u8) -> Color {
    match n {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        _ => Color::White,
    }
}

/// Minimal SGR parser: colours (16/256/truecolor), bold/dim/italic/underline.
pub fn parse_ansi(s: &str) -> Line<'static> {
    parse_ansi_tabs(s, true)
}

/// [`parse_ansi`], optionally keeping tabs as they are (so char positions
/// still match the source text).
pub fn parse_ansi_tabs(s: &str, expand_tabs: bool) -> Line<'static> {
    let mut spans = Vec::new();
    let mut style = Style::default();
    let mut buf = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                let mut params = String::new();
                let mut fin = ' ';
                for d in chars.by_ref() {
                    if d.is_ascii_digit() || d == ';' || d == ':' {
                        params.push(d);
                    } else {
                        fin = d;
                        break;
                    }
                }
                if fin != 'm' {
                    continue;
                }
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), style));
                }
                let nums: Vec<u16> = params.split([';', ':']).map(|p| p.parse().unwrap_or(0)).collect();
                let mut i = 0;
                while i < nums.len() {
                    match nums[i] {
                        0 => style = Style::default(),
                        1 => style = style.add_modifier(Modifier::BOLD),
                        2 => style = style.add_modifier(Modifier::DIM),
                        3 => style = style.add_modifier(Modifier::ITALIC),
                        4 => style = style.add_modifier(Modifier::UNDERLINED),
                        7 => style = style.add_modifier(Modifier::REVERSED),
                        22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
                        23 => style = style.remove_modifier(Modifier::ITALIC),
                        24 => style = style.remove_modifier(Modifier::UNDERLINED),
                        27 => style = style.remove_modifier(Modifier::REVERSED),
                        n @ 30..=37 => style = style.fg(ansi_color((n - 30) as u8)),
                        n @ 90..=97 => style = style.fg(ansi_color((n - 90 + 8) as u8)),
                        n @ 40..=47 => style = style.bg(ansi_color((n - 40) as u8)),
                        n @ 100..=107 => style = style.bg(ansi_color((n - 100 + 8) as u8)),
                        39 => style.fg = None,
                        49 => style.bg = None,
                        n @ (38 | 48) => {
                            let col = match nums.get(i + 1) {
                                Some(5) => {
                                    i += 2;
                                    nums.get(i).map(|&v| Color::Indexed(v as u8))
                                }
                                Some(2) => {
                                    i += 4;
                                    match (nums.get(i - 2), nums.get(i - 1), nums.get(i)) {
                                        (Some(&r), Some(&g), Some(&b)) => Some(Color::Rgb(r as u8, g as u8, b as u8)),
                                        _ => None,
                                    }
                                }
                                _ => None,
                            };
                            if let Some(col) = col {
                                style = if n == 38 { style.fg(col) } else { style.bg(col) };
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
            continue;
        }
        if c == '\t' {
            buf.push_str(if expand_tabs { "    " } else { "\t" });
        } else if !c.is_control() {
            buf.push(c);
        }
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, style));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_truecolor_and_reset() {
        let l = parse_ansi("\x1b[38;2;1;2;3mab\x1b[0m c\x1b[1;31md");
        assert_eq!(l.spans.len(), 3);
        assert_eq!(l.spans[0].content, "ab");
        assert_eq!(l.spans[0].style.fg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(l.spans[1].style, Style::default());
        assert_eq!(l.spans[2].style.fg, Some(Color::Red));
    }

    #[test]
    fn diff_before_first_commit() {
        let t = crate::plan::tests::tmpdir("preview-unborn");
        let git = |args: &[&str]| Command::new("git").args(args).current_dir(&t.0).output().unwrap();
        git(&["init", "-q"]);
        std::fs::write(t.0.join("a.txt"), "hello\n").unwrap();
        git(&["add", "a.txt"]);
        let v = git_diff(&t.0.join("a.txt"), &Theme::load(), 80, 100).expect("staged diff");
        assert!(v[0].to_string().contains("a.txt"));
        assert!(v.iter().any(|l| l.to_string().contains("+ hello")), "{v:?}");
    }

    #[test]
    fn worker_renders_in_background() {
        let t = crate::plan::tests::tmpdir("preview-worker");
        std::fs::write(t.0.join("a.txt"), "hello\n").unwrap();
        let mut c = Cache::default();
        let th = Theme::load();
        c.get(&t.0.join("a.txt"), None, 40, &th, false, 10, Show::File);
        let t0 = std::time::Instant::now();
        while !c.ready() && t0.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(5));
            c.poll();
        }
        assert!(c.lines.iter().any(|l| l.to_string().contains("hello")));
        c.get_now(&t.0.join("a.txt"), None, 40, &th, false, 10, Show::File);
        assert!(c.ready());
    }

    #[test]
    fn pdf_without_tools_still_previews() {
        let t = crate::plan::tests::tmpdir("preview-pdf");
        std::fs::write(t.0.join("x.pdf"), b"%PDF-1.4\n%\xe2\xe3\n").unwrap();
        let (v, _) = render(&t.0.join("x.pdf"), None, 60, &Theme::load(), false, 50);
        assert!(v[0].to_string().contains("PDF"), "{v:?}");
    }
}
