//! Preview pane content: syntax-highlighted text through `bat` when it is
//! installed (ANSI parsed into ratatui spans), plain numbered text
//! otherwise, a listing for directories, and a summary for binaries.

use crate::entry::{self, Kind, Sort, Store};
use crate::theme::Theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

#[derive(Clone, PartialEq, Eq)]
pub struct Key {
    pub path: PathBuf,
    pub line: Option<usize>,
    pub width: u16,
    pub mtime: Option<std::time::SystemTime>,
}

pub struct Cache {
    pub key: Option<Key>,
    pub lines: Vec<Line<'static>>,
    /// Index into `lines` to scroll to (grep hit).
    pub focus: usize,
}

impl Default for Cache {
    fn default() -> Self {
        Cache { key: None, lines: vec![], focus: 0 }
    }
}

fn bat_bin() -> Option<&'static str> {
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

const MAX_LINES: usize = 400;

impl Cache {
    pub fn get(&mut self, path: &Path, line: Option<usize>, width: u16, th: &Theme, hidden: bool) -> &Cache {
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let key = Key { path: path.to_path_buf(), line, width, mtime };
        if self.key.as_ref() != Some(&key) {
            let (lines, focus) = render(path, line, width, th, hidden);
            self.lines = lines;
            self.focus = focus;
            self.key = Some(key);
        }
        self
    }
}

fn render(path: &Path, line: Option<usize>, width: u16, th: &Theme, hidden: bool) -> (Vec<Line<'static>>, usize) {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return (vec![Line::styled(format!(" {e}"), Style::default().fg(th.red))], 0),
    };
    if meta.is_dir() {
        return (dir_listing(path, th, hidden), 0);
    }
    // binary sniff
    let mut head = [0u8; 8192];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    if head[..n].contains(&0) {
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let mut v = vec![
            Line::styled(" binary file", Style::default().fg(th.subtext).add_modifier(Modifier::BOLD)),
            Line::styled(format!(" {}  ·  {ext}", entry::human_size(meta.len())), Style::default().fg(th.dim)),
        ];
        if cfg!(target_os = "macos") {
            if let Ok(o) = Command::new("file").arg("-b").arg(path).output() {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                v.push(Line::raw(""));
                v.push(Line::styled(format!(" {s}"), Style::default().fg(th.dim)));
            }
        }
        return (v, 0);
    }
    let (start, end) = match line {
        Some(l) => (l.saturating_sub(MAX_LINES / 4).max(1), l + MAX_LINES * 3 / 4),
        None => (1, MAX_LINES),
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

fn dir_listing(path: &Path, th: &Theme, hidden: bool) -> Vec<Line<'static>> {
    let mut store = Store::default();
    let ids = match store.list(path, hidden, Sort::Name) {
        Ok(v) => v,
        Err(e) => return vec![Line::styled(format!(" {e}"), Style::default().fg(th.red))],
    };
    if ids.is_empty() {
        return vec![Line::styled(" (empty directory)", Style::default().fg(th.dim))];
    }
    ids.iter()
        .take(MAX_LINES)
        .filter_map(|id| store.get(*id))
        .map(|e| {
            let (ic, col) = entry::icon(&e.name, e.kind);
            let fg = if e.kind == Kind::Dir { th.accent } else { th.text };
            Line::from(vec![
                Span::styled(format!(" {ic} "), Style::default().fg(crate::ui::icon_color(th, col))),
                Span::styled(e.display(), Style::default().fg(fg)),
            ])
        })
        .collect()
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
            buf.push_str("    ");
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
}
