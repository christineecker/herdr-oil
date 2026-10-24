//! Rendering: explorer (editable listing + optional preview), finder
//! (tabs, prompt, results, preview), overlays and the status line.

use crate::app::{App, MsgKind, Overlay, View};
use crate::entry::{self, IconColor, Kind};
use crate::finder::tilde;
use crate::plan::{self, Op};
use std::collections::HashMap;
use crate::theme::Theme;
use crate::vim::{FinderKind, Mode};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use std::path::Path;
use unicode_width::UnicodeWidthStr;

pub fn icon_color(th: &Theme, c: IconColor) -> Color {
    match c {
        IconColor::Accent => th.accent,
        IconColor::Text => th.subtext,
        IconColor::Dim => th.dim,
        IconColor::Peach => th.peach,
        IconColor::Green => th.green,
        IconColor::Yellow => th.yellow,
        IconColor::Blue => th.blue,
        IconColor::Teal => th.teal,
        IconColor::Red => th.red,
    }
}

const SPIN: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [main, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    match app.view {
        View::Explorer => draw_explorer(f, app, main),
        View::Finder => draw_finder(f, app, main),
        View::Viewer => draw_viewer(f, app, main),
    }
    draw_status(f, app, status);
    match &app.overlay {
        Overlay::None => {}
        Overlay::Confirm { .. } => draw_confirm(f, app, area),
        Overlay::Help { scroll } => draw_help(f, app, area, *scroll),
        Overlay::QuitPrompt => draw_quit(f, app, area),
        Overlay::Report { title, lines, error, scroll } => draw_report(f, &app.th, area, title, lines, *error, *scroll),
    }
}

fn split_preview(area: Rect, on: bool) -> (Rect, Option<Rect>) {
    if !on || area.width < 50 {
        return (area, None);
    }
    let [l, r] = Layout::horizontal([Constraint::Percentage(48), Constraint::Percentage(52)]).areas(area);
    (l, Some(r))
}

// ── explorer ────────────────────────────────────────────────────────────────

fn draw_explorer(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.th.clone();
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);

    // header: path + pending-change summary (from the real plan)
    let modified = app.modified_dirs();
    let pending = if modified.is_empty() { None } else { Some(plan::build(app.buffers.values(), &app.store)) };
    let mut tags: HashMap<String, char> = HashMap::new();
    let mut summary: Vec<Span> = Vec::new();
    if let Some(p) = &pending {
        let (mut c, mut r, mut m, mut cp, mut d) = (0, 0, 0, 0, 0);
        for op in &p.ops {
            match op {
                Op::Create { dst, .. } => {
                    c += 1;
                    tags.insert(plan::key(dst), '+');
                }
                Op::Move { src, dst, .. } => {
                    if src.parent() == dst.parent() {
                        r += 1;
                        tags.insert(plan::key(dst), '~');
                    } else {
                        m += 1;
                        tags.insert(plan::key(dst), '→');
                    }
                }
                Op::Copy { dst, .. } => {
                    cp += 1;
                    tags.insert(plan::key(dst), '⧉');
                }
                Op::Delete { .. } => d += 1,
            }
        }
        for (n, sym, col) in [(c, "+", th.green), (r, "~", th.yellow), (m, "→", th.blue), (cp, "⧉", th.blue), (d, "−", th.red)] {
            if n > 0 {
                summary.push(Span::styled(format!(" {sym}{n}"), Style::default().fg(col).add_modifier(Modifier::BOLD)));
            }
        }
        if !p.errors.is_empty() {
            summary.push(Span::styled(format!("  ⚠ {} problem(s)", p.errors.len()), Style::default().fg(th.red).add_modifier(Modifier::BOLD)));
        }
        let others = modified.iter().filter(|d| **d != app.cwd).count();
        if others > 0 {
            summary.push(Span::styled(format!("  ({others} other dir{})", if others == 1 { "" } else { "s" }), Style::default().fg(th.peach)));
        }
        if !summary.is_empty() {
            summary.insert(0, Span::styled("  pending", Style::default().fg(th.dim)));
            summary.push(Span::styled("  :w", Style::default().fg(th.accent)));
        }
    }
    let right = format!(
        "{}{}{} ",
        app.sort.label(),
        if app.hidden { " · hidden" } else { "" },
        if app.details == 2 { " · details" } else { "" }
    );
    let dirty = if app.buf().modified() { " [+]" } else { "" };
    let sum_w: usize = summary.iter().map(|s| s.content.width()).sum();
    let budget = (head.width as usize).saturating_sub(3 + sum_w + right.width() + dirty.width() + 1).max(8);
    let mut path = format!("{}/", tilde(&app.cwd).trim_end_matches('/'));
    if path.width() > budget {
        let tail: String = path.chars().rev().take(budget.saturating_sub(1)).collect::<Vec<_>>().into_iter().rev().collect();
        path = format!("…{tail}");
    }
    let (pre, last) = match path.trim_end_matches('/').rfind('/') {
        Some(i) => (path[..=i].to_string(), path[i + 1..].to_string()),
        None => (String::new(), path.clone()),
    };
    let mut hs = vec![
        Span::styled(" \u{f07c} ", Style::default().fg(th.accent)),
        Span::styled(pre, Style::default().fg(th.subtext)),
        Span::styled(last, Style::default().fg(th.text).add_modifier(Modifier::BOLD)),
        Span::styled(dirty, Style::default().fg(th.yellow).add_modifier(Modifier::BOLD)),
    ];
    hs.extend(summary);
    let used: usize = hs.iter().map(|s| s.content.width()).sum();
    let pad = (head.width as usize).saturating_sub(used + right.width());
    hs.push(Span::raw(" ".repeat(pad)));
    hs.push(Span::styled(right, Style::default().fg(th.dim)));
    f.render_widget(Paragraph::new(Line::from(hs)), head);

    let (list, prev) = split_preview(body, app.preview_on);
    let list_inner = if prev.is_some() {
        let blk = Block::default().borders(Borders::RIGHT).border_style(Style::default().fg(th.dim)).border_type(BorderType::Plain);
        let inner = blk.inner(list);
        f.render_widget(blk, list);
        inner
    } else {
        list
    };
    app.list_area = list_inner;
    let mode = app.vim.mode.clone();
    let (details, icons) = (app.details, app.cfg.icons);
    let cwd = app.cwd.clone();
    let store = &app.store;
    let b = app.buffers.get_mut(&cwd).unwrap();
    b.view_h = list_inner.height as usize;
    b.follow();
    let original: std::collections::HashSet<u64> = b.original.iter().copied().collect();
    let vis = match mode {
        Mode::Visual { anchor } => Some((anchor.min(b.row), anchor.max(b.row))),
        _ => None,
    };
    let mut lines = Vec::new();
    let mut cursor_pos = None;
    let w = list_inner.width as usize;
    if let Some(e) = &b.error {
        lines.push(Line::styled(format!("  {e}"), Style::default().fg(th.red)));
    }
    let empty_dir = b.lines.len() == 1 && !b.lines[0].significant() && b.error.is_none();
    for (i, l) in b.lines.iter().enumerate().skip(b.scroll).take(list_inner.height as usize) {
        let entry = l.id.and_then(|id| store.get(id));
        let name_now = l.text.trim().trim_end_matches('/');
        // change marker, straight from the plan
        let untouched = entry.is_some_and(|e| e.name == name_now && e.path.parent() == Some(b.dir.as_path()));
        let (mark, mcol) = if !l.significant() || (untouched && original.contains(&l.id.unwrap_or(0))) {
            (" ", th.dim)
        } else {
            match plan::parse_name(&l.text) {
                Err(_) => ("!", th.red),
                Ok(None) => (" ", th.dim),
                Ok(Some((rel, _))) => match tags.get(&plan::key(&plan::normalize(&b.dir.join(rel)))) {
                    Some('+') => ("+", th.green),
                    Some('~') => ("~", th.yellow),
                    Some('→') => ("→", th.blue),
                    Some('⧉') => ("⧉", th.blue),
                    _ if l.id.is_some() && entry.is_none() => ("?", th.red),
                    _ => (" ", th.dim),
                },
            }
        };
        let gitc = entry.filter(|e| e.name == name_now).and_then(|e| b.git.get(&e.name).copied());
        let gcol = match gitc {
            Some('M') | Some('R') => th.yellow,
            Some('A') => th.green,
            Some('?') => th.teal,
            Some('D') | Some('U') => th.red,
            _ => th.dim,
        };
        let mut spans = vec![
            Span::styled(mark.to_string(), Style::default().fg(mcol).add_modifier(Modifier::BOLD)),
            Span::styled(gitc.map(|c| c.to_string()).unwrap_or_else(|| " ".into()), Style::default().fg(gcol)),
            Span::raw(" "),
        ];
        let kind = entry.map(|e| e.kind).unwrap_or(if l.text.trim_end().ends_with('/') { Kind::Dir } else { Kind::File });
        let mut used = 3;
        if icons {
            let (ic, col) = if l.significant() { entry::icon(name_now, kind) } else { (" ", IconColor::Dim) };
            spans.push(Span::styled(format!("{ic} "), Style::default().fg(icon_color(&th, col))));
            used += 2 + ic.width().saturating_sub(1);
        } else {
            spans.push(Span::raw(" "));
            used += 1;
        }
        let base = match kind {
            _ if l.id.is_none() => Style::default().fg(th.green),
            Kind::Dir | Kind::LinkDir => Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
            Kind::Link => Style::default().fg(th.teal),
            Kind::Other => Style::default().fg(th.dim),
            Kind::File if entry.is_some_and(|e| e.is_exec()) => Style::default().fg(th.green),
            Kind::File if name_now.starts_with('.') => Style::default().fg(th.subtext),
            Kind::File => Style::default().fg(th.text),
        };
        let base = if gitc == Some('!') { base.add_modifier(Modifier::DIM) } else { base };
        // details (right side, virtual)
        let mut tail = Vec::new();
        if let Some(e) = entry {
            if let Some(t) = &e.link_target {
                tail.push(Span::styled(format!(" → {t}"), Style::default().fg(th.dim)));
            }
            let size = entry::size_label(e);
            // big files stand out: K/B dim, M subtext, G+ peach
            let scol = match size.chars().last() {
                Some('G') | Some('T') => th.peach,
                Some('M') => th.subtext,
                _ => th.dim,
            };
            if details == 2 && w > 50 {
                tail.push(Span::styled(format!("  {}  ", entry::perms(e.mode, e.kind)), Style::default().fg(th.dim)));
                tail.push(Span::styled(format!("{size:>9}"), Style::default().fg(scol)));
                tail.push(Span::styled(format!("  {:>4}", entry::age(e.mtime)), Style::default().fg(th.dim)));
            } else if details == 1 && w > 30 {
                tail.push(Span::styled(format!("  {size:>9}"), Style::default().fg(scol)));
            }
        }
        let tail_w: usize = tail.iter().map(|s| s.content.width()).sum();
        let avail = w.saturating_sub(used + 1).max(4);
        let chars: Vec<char> = l.text.chars().collect();
        let is_cur = i == b.row;
        // horizontal scroll for the cursor line
        let mut off = 0;
        if is_cur {
            if b.col >= avail {
                off = b.col + 1 - avail;
            }
            b.hscroll = off;
        }
        let shown: Vec<char> = chars.iter().skip(off).take(avail).copied().collect();
        let in_vis = vis.is_some_and(|(a, z)| i >= a && i <= z);
        let row_bg = if in_vis {
            Some(th.surface)
        } else if is_cur && matches!(mode, Mode::Normal | Mode::Cmd { .. }) {
            Some(th.selection)
        } else {
            None
        };
        if is_cur && matches!(mode, Mode::Normal) {
            let c = b.col.saturating_sub(off);
            let pre: String = shown.iter().take(c).collect();
            let at: String = shown.get(c).map(|x| x.to_string()).unwrap_or_else(|| " ".into());
            let post: String = shown.iter().skip(c + 1).collect();
            spans.push(Span::styled(pre, base));
            spans.push(Span::styled(at, base.add_modifier(Modifier::REVERSED)));
            spans.push(Span::styled(post, base));
        } else {
            let s: String = shown.iter().collect();
            if empty_dir && i == 0 && !matches!(mode, Mode::Insert) {
                spans.push(Span::styled("(empty — o to add a file, name/ for a dir)", Style::default().fg(th.dim).add_modifier(Modifier::ITALIC)));
            } else {
                spans.push(Span::styled(s, base));
            }
        }
        if is_cur && matches!(mode, Mode::Insert) {
            let x = list_inner.x + used as u16 + (b.col - off) as u16;
            cursor_pos = Some(Position::new(x.min(list_inner.right().saturating_sub(1)), list_inner.y + (i - b.scroll) as u16));
        }
        let text_w = shown.len().max(if is_cur { 1 } else { 0 });
        if !tail.is_empty() && used + text_w + tail_w < w {
            let (link, det): (Vec<_>, Vec<_>) = tail.into_iter().partition(|s| s.content.starts_with(" →"));
            let lw: usize = link.iter().map(|s| s.content.width()).sum();
            let dw: usize = det.iter().map(|s| s.content.width()).sum();
            spans.extend(link);
            if !det.is_empty() {
                spans.push(Span::raw(" ".repeat(w.saturating_sub(used + text_w + lw + dw))));
                spans.extend(det);
            }
        }
        let mut line = Line::from(spans);
        if let Some(bg) = row_bg {
            line = line.style(Style::default().bg(bg));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), list_inner);
    if let Some(p) = cursor_pos {
        f.set_cursor_position(p);
    }
    if let Some(pr) = prev {
        let target = b.cur_entry(store).map(|e| e.path.clone());
        draw_preview(f, app, pr, target.as_deref(), None);
    }
}

fn draw_preview(f: &mut Frame, app: &mut App, area: Rect, path: Option<&Path>, line: Option<usize>) {
    let th = app.th.clone();
    let [title, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let Some(path) = path else {
        f.render_widget(Paragraph::new(Line::styled("  no preview", Style::default().fg(th.dim))), body);
        return;
    };
    let meta = std::fs::symlink_metadata(path).ok();
    let mut t = vec![Span::styled(format!(" {}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()), Style::default().fg(th.text).add_modifier(Modifier::BOLD))];
    if let Some(m) = &meta {
        if !m.is_dir() {
            t.push(Span::styled(format!("  {}", entry::human_size(m.len())), Style::default().fg(th.dim)));
        }
        t.push(Span::styled(format!("  {}", entry::age(m.modified().ok())), Style::default().fg(th.dim)));
    }
    if let Some(l) = line {
        t.push(Span::styled(format!("  :{l}"), Style::default().fg(th.teal)));
    }
    f.render_widget(Paragraph::new(Line::from(t)), title);
    let hidden = app.hidden;
    let c = app.preview.get(path, line, body.width, &th, hidden, crate::preview::PREVIEW_LINES);
    let base = c.focus.saturating_sub(body.height as usize / 3);
    let start = (base + app.preview_scroll).min(c.lines.len().saturating_sub(1));
    let lines: Vec<Line> = c.lines.iter().skip(start).take(body.height as usize).cloned().collect();
    f.render_widget(Paragraph::new(lines), Rect { x: body.x + 1, width: body.width.saturating_sub(1), ..body });
}

// ── viewer ──────────────────────────────────────────────────────────────────

fn draw_viewer(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.th.clone();
    let hidden = app.hidden;
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let Some(v) = app.viewer.as_mut() else { return };
    let c = app.viewer_cache.get(&v.path, v.line, body.width.saturating_sub(1), &th, hidden, crate::preview::VIEWER_LINES);
    let h = body.height as usize;
    v.height = h;
    v.total = c.lines.len();
    let max_top = v.total.saturating_sub(h);
    let top = v.top.unwrap_or_else(|| c.focus.saturating_sub(h / 3)).min(max_top);
    v.top = Some(top);
    let name = v.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = v.path.parent().map(tilde).unwrap_or_default();
    let meta = std::fs::metadata(&v.path).ok();
    let (ic, col) = entry::icon(&name, Kind::File);
    let pos = if v.total == 0 { String::new() } else { format!("{}–{} of {} ", top + 1, (top + h).min(v.total), v.total) };
    let mut hs = vec![
        Span::styled(format!(" {ic} "), Style::default().fg(icon_color(&th, col))),
        Span::styled(format!("{dir}/"), Style::default().fg(th.subtext)),
        Span::styled(name, Style::default().fg(th.text).add_modifier(Modifier::BOLD)),
    ];
    if let Some(m) = &meta {
        hs.push(Span::styled(format!("  {}  {}", entry::human_size(m.len()), entry::age(m.modified().ok())), Style::default().fg(th.dim)));
    }
    let used: usize = hs.iter().map(|s| s.content.width()).sum();
    hs.push(Span::raw(" ".repeat((head.width as usize).saturating_sub(used + pos.width()))));
    hs.push(Span::styled(pos, Style::default().fg(th.dim)));
    f.render_widget(Paragraph::new(Line::from(hs)), head);
    let lines: Vec<Line> = c.lines.iter().skip(top).take(h).cloned().collect();
    app.list_area = body;
    f.render_widget(Paragraph::new(lines), Rect { x: body.x + 1, width: body.width.saturating_sub(1), ..body });
}

// ── finder ──────────────────────────────────────────────────────────────────

fn highlight(s: &str, idx: &[u32], base: Style, hl: Style) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_hl = false;
    for (i, ch) in s.chars().enumerate() {
        let h = idx.binary_search(&(i as u32)).is_ok();
        if h != cur_hl && !cur.is_empty() {
            out.push(Span::styled(std::mem::take(&mut cur), if cur_hl { hl } else { base }));
        }
        cur_hl = h;
        cur.push(ch);
    }
    if !cur.is_empty() {
        out.push(Span::styled(cur, if cur_hl { hl } else { base }));
    }
    out
}

fn substr_indices(text: &str, q: &str) -> Vec<u32> {
    if q.is_empty() {
        return vec![];
    }
    let smart = q.chars().any(|c| c.is_uppercase());
    let (t, qq) = if smart { (text.to_string(), q.to_string()) } else { (text.to_lowercase(), q.to_lowercase()) };
    let tc: Vec<char> = t.chars().collect();
    let qc: Vec<char> = qq.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i + qc.len() <= tc.len() {
        if tc[i..i + qc.len()] == qc[..] {
            out.extend((i..i + qc.len()).map(|x| x as u32));
            i += qc.len();
        } else {
            i += 1;
        }
    }
    out
}

fn draw_finder(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.th.clone();
    let [tabs, prompt, body] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let spin = SPIN[app.spinner % SPIN.len()];
    let preview_on = app.finder_preview && area.width >= 60;
    let Some(fd) = app.finder.as_mut() else { return };

    // tabs
    let mut ts = vec![Span::raw(" ")];
    for (k, label, ic) in [
        (FinderKind::Files, "Files", "\u{f0214}"),
        (FinderKind::Dirs, "Dirs", "\u{f024b}"),
        (FinderKind::Grep, "Grep", "\u{f0349}"),
        (FinderKind::Recent, "Recent", "\u{f02da}"),
    ] {
        let st = if fd.kind == k {
            Style::default().fg(Color::Black).bg(th.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.subtext)
        };
        ts.push(Span::styled(format!(" {ic} {label} "), st));
        ts.push(Span::raw(" "));
    }
    let mut flags = String::new();
    if fd.hidden {
        flags.push_str(" +hidden");
    }
    if fd.no_ignore {
        flags.push_str(" +ignored");
    }
    let count = if fd.kind == FinderKind::Grep { format!("{} hits", fd.len()) } else { format!("{}/{}", fd.len(), fd.total()) };
    let trunc = if fd.truncated() { " (capped)" } else { "" };
    let used: usize = ts.iter().map(|s| s.content.width()).sum();
    let lead = format!("{}{}  \u{f07b} ", if fd.walking() { format!("{spin} ") } else { String::new() }, count);
    let budget = (tabs.width as usize).saturating_sub(used + lead.width() + flags.width() + trunc.len() + 3).max(6);
    let mut root = tilde(&fd.root);
    if root.width() > budget {
        let tail: String = root.chars().rev().take(budget - 1).collect::<Vec<_>>().into_iter().rev().collect();
        root = format!("…{tail}");
    }
    let right = format!("{lead}{root}{flags}  ");
    let pad = (tabs.width as usize).saturating_sub(used + right.width() + trunc.len());
    ts.push(Span::raw(" ".repeat(pad)));
    ts.push(Span::styled(right, Style::default().fg(th.dim)));
    ts.push(Span::styled(trunc, Style::default().fg(th.peach)));
    f.render_widget(Paragraph::new(Line::from(ts)), tabs);

    // prompt
    let ph = match fd.kind {
        FinderKind::Grep => "search file contents (ripgrep, smart-case)…",
        FinderKind::Dirs => "fuzzy-find directories…",
        FinderKind::Recent => "recently opened files…",
        FinderKind::Files => "fuzzy-find files  ('exact ^prefix suffix$ !not)",
    };
    let mut ps = vec![Span::styled(" \u{f0349} ", Style::default().fg(th.accent).add_modifier(Modifier::BOLD))];
    if fd.query.is_empty() {
        ps.push(Span::styled(ph, Style::default().fg(th.dim).add_modifier(Modifier::ITALIC)));
    } else {
        ps.push(Span::styled(fd.query.clone(), Style::default().fg(th.text)));
    }
    if !fd.marked.is_empty() {
        ps.push(Span::styled(format!("   ● {} marked", fd.marked.len()), Style::default().fg(th.peach)));
    }
    f.render_widget(Paragraph::new(Line::from(ps)), prompt);
    let qx: usize = fd.query.chars().take(fd.qcur).map(|c| c.to_string().width()).sum();
    if matches!(app.overlay, Overlay::None) {
        f.set_cursor_position(Position::new(prompt.x + 3 + qx as u16, prompt.y));
    }

    let [sep, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(body);
    let (list, prev) = split_preview(rest, preview_on);
    let mut rule: String = "─".repeat(sep.width as usize);
    let list_inner = if prev.is_some() {
        let jx = (list.width.saturating_sub(1)) as usize;
        rule = rule.chars().enumerate().map(|(i, c)| if i == jx { '┬' } else { c }).collect();
        let blk = Block::default().borders(Borders::RIGHT).border_style(Style::default().fg(th.dim));
        let inner = blk.inner(list);
        f.render_widget(blk, list);
        inner
    } else {
        list
    };
    f.render_widget(Paragraph::new(Line::styled(rule, Style::default().fg(th.dim))), sep);
    let h = list_inner.height as usize;
    fd.follow(h);
    let (scroll, sel) = (fd.scroll, fd.sel);
    let items = fd.items(scroll, scroll + h);
    let marked = fd.marked.clone();
    let query = fd.query.clone();
    let kind = fd.kind;
    let w = list_inner.width as usize;
    let mut lines = Vec::new();
    let hl = Style::default().fg(th.yellow).add_modifier(Modifier::BOLD);
    for (k, it) in items.iter().enumerate() {
        let is_sel = scroll + k == sel;
        let mut sp = vec![Span::styled(
            if is_sel { "▌" } else { " " },
            Style::default().fg(th.accent),
        )];
        sp.push(Span::styled(if marked.contains(&it.abs) { "●" } else { " " }, Style::default().fg(th.peach)));
        let name = it.display.trim_end_matches('/');
        let base_name = name.rsplit('/').next().unwrap_or(name);
        let (ic, col) = entry::icon(base_name, if it.is_dir { Kind::Dir } else { Kind::File });
        sp.push(Span::styled(format!("{ic} "), Style::default().fg(icon_color(&th, col))));
        match &it.grep_text {
            Some(text) => {
                sp.push(Span::styled(it.display.clone(), Style::default().fg(th.blue)));
                sp.push(Span::styled(format!(":{}", it.line.unwrap_or(0)), Style::default().fg(th.dim)));
                sp.push(Span::raw("  "));
                let t = text.trim_start();
                let room = w.saturating_sub(sp.iter().map(|s| s.content.width()).sum::<usize>());
                let t: String = t.chars().take(room).collect();
                let idx = substr_indices(&t, &query);
                sp.extend(highlight(&t, &idx, Style::default().fg(th.text), hl));
            }
            None => {
                // dim directory part, bright file name
                let split = it.display.trim_end_matches('/').rfind('/').map(|i| i + 1).unwrap_or(0);
                let dir_chars = it.display[..split].chars().count() as u32;
                let (dpart, fpart) = it.display.split_at(split);
                let didx: Vec<u32> = it.indices.iter().copied().filter(|&i| i < dir_chars).collect();
                let fidx: Vec<u32> = it.indices.iter().copied().filter(|&i| i >= dir_chars).map(|i| i - dir_chars).collect();
                sp.extend(highlight(dpart, &didx, Style::default().fg(th.subtext), hl));
                let fstyle = if it.is_dir { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.text) };
                sp.extend(highlight(fpart, &fidx, fstyle, hl));
            }
        }
        let mut line = Line::from(sp);
        if is_sel {
            line = line.style(Style::default().bg(th.selection));
        }
        lines.push(line);
    }
    if items.is_empty() {
        let msg = match kind {
            FinderKind::Grep if query.is_empty() => "type to search inside files",
            FinderKind::Recent => "nothing opened through herdr-oil yet",
            _ => "no matches",
        };
        lines.push(Line::styled(format!("  {msg}"), Style::default().fg(th.dim).add_modifier(Modifier::ITALIC)));
    }
    app.list_area = list_inner;
    f.render_widget(Paragraph::new(lines), list_inner);
    if let Some(p) = prev {
        let sel_item = items.into_iter().nth(sel.saturating_sub(scroll)).filter(|_| sel >= scroll);
        let inner = p;
        match sel_item {
            Some(it) => draw_preview(f, app, inner, Some(&it.abs), it.line),
            None => draw_preview(f, app, inner, None, None),
        }
    }
}

// ── status line ─────────────────────────────────────────────────────────────

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.th;
    if let (View::Explorer, Mode::Cmd { kind, text, cur, .. }) = (app.view, &app.vim.mode) {
        let line = Line::from(vec![Span::styled(kind.to_string(), Style::default().fg(th.accent).add_modifier(Modifier::BOLD)), Span::styled(text.clone(), Style::default().fg(th.text))]);
        f.render_widget(Paragraph::new(line), area);
        let x: usize = text.chars().take(*cur).map(|c| c.to_string().width()).sum();
        f.set_cursor_position(Position::new(area.x + 1 + x as u16, area.y));
        return;
    }
    let (label, bg) = match (app.view, &app.vim.mode) {
        (View::Finder, _) => (" FIND ", th.blue),
        (View::Viewer, _) => (" VIEW ", th.teal),
        (_, Mode::Normal) => (" OIL ", th.accent),
        (_, Mode::Insert) => (" INSERT ", th.green),
        (_, Mode::Visual { .. }) => (" V-LINE ", th.peach),
        (_, Mode::Cmd { .. }) => (" CMD ", th.yellow),
    };
    let mut sp = vec![Span::styled(label, Style::default().fg(Color::Black).bg(bg).add_modifier(Modifier::BOLD)), Span::raw(" ")];
    let pend = app.vim.pending_str();
    if !pend.is_empty() && app.view == View::Explorer {
        sp.push(Span::styled(format!("{} ", pend.replace(' ', "␣")), Style::default().fg(th.yellow)));
    }
    if let Some((m, k, _)) = &app.msg {
        let c = match k {
            MsgKind::Info => th.subtext,
            MsgKind::Ok => th.green,
            MsgKind::Warn => th.yellow,
            MsgKind::Err => th.red,
        };
        sp.push(Span::styled(m.clone(), Style::default().fg(c)));
    }
    let hints: &[(&str, &str)] = match (app.view, &app.vim.mode) {
        (View::Viewer, _) => &[("j/k ␣ b", "scroll"), ("e", "edit here"), ("^v", "split"), ("^t", "tab"), ("y", "copy path"), ("q", "back")],
        (View::Finder, _) => &[("⏎", "open"), ("^v", "split"), ("^t", "tab"), ("^o", "reveal"), ("⇥", "mode"), ("^r", "root"), ("esc", "back")],
        (_, Mode::Insert) => &[("esc", "normal"), ("⏎", "new line"), ("^s", "save")],
        (_, Mode::Visual { .. }) => &[("d", "cut"), ("y", "yank"), ("c", "change"), ("esc", "cancel")],
        _ => &[("⏎", "open"), ("-", "up"), (":w", "apply"), ("⇥", "find"), ("␣/", "grep"), ("g?", "help"), ("esc", "close")],
    };
    let mut hs = Vec::new();
    for (k, v) in hints {
        hs.push(Span::styled(format!(" {k}"), Style::default().fg(th.accent)));
        hs.push(Span::styled(format!(" {v}"), Style::default().fg(th.dim)));
    }
    let used: usize = sp.iter().map(|s| s.content.width()).sum();
    let hw: usize = hs.iter().map(|s| s.content.width()).sum();
    if used + hw + 1 < area.width as usize {
        sp.push(Span::raw(" ".repeat(area.width as usize - used - hw - 1)));
        sp.extend(hs);
    }
    f.render_widget(Paragraph::new(Line::from(sp)), area);
}

// ── overlays ────────────────────────────────────────────────────────────────

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2)).max(10);
    let h = h.min(area.height.saturating_sub(2)).max(3);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn frame<'a>(th: &Theme, title: &'a str, color: Color) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .title(Span::styled(format!(" {title} "), Style::default().fg(color).add_modifier(Modifier::BOLD)))
        .style(Style::default().bg(th.surface_bg()))
}

/// Deepest directory containing every path the plan touches.
fn common_base(plan: &plan::Plan) -> std::path::PathBuf {
    let mut base: Option<std::path::PathBuf> = None;
    for op in &plan.ops {
        for p in [op.src(), op.dst()].into_iter().flatten() {
            let parent = p.parent().unwrap_or(Path::new("/")).to_path_buf();
            base = Some(match base {
                None => parent,
                Some(b) => {
                    let mut c = std::path::PathBuf::new();
                    for (x, y) in b.components().zip(parent.components()) {
                        if x != y {
                            break;
                        }
                        c.push(x.as_os_str());
                    }
                    c
                }
            });
        }
    }
    base.unwrap_or_else(|| std::path::PathBuf::from("/"))
}

fn rel(base: &Path, p: &Path) -> String {
    match p.strip_prefix(base) {
        Ok(r) if !r.as_os_str().is_empty() => r.display().to_string(),
        _ => tilde(p),
    }
}

fn draw_confirm(f: &mut Frame, app: &App, area: Rect) {
    let Overlay::Confirm { plan, scroll, .. } = &app.overlay else { return };
    let th = &app.th;
    let trash = matches!(app.cfg.delete, crate::config::DeleteMethod::Trash);
    let base = common_base(plan);
    let mut lines = Vec::new();
    for op in &plan.ops {
        let (tag, col, text) = match op {
            Op::Create { dst, dir } => ("CREATE", th.green, format!("{}{}", rel(&base, dst), if *dir { "/" } else { "" })),
            Op::Move { src, dst, dir } => {
                let s = if *dir { "/" } else { "" };
                let same_dir = src.parent() == dst.parent();
                (if same_dir { "RENAME" } else { "MOVE" }, th.yellow, format!("{}{s}  →  {}{s}", rel(&base, src), rel(&base, dst)))
            }
            Op::Copy { src, dst, dir } => {
                let s = if *dir { "/" } else { "" };
                ("COPY", th.blue, format!("{}{s}  →  {}{s}", rel(&base, src), rel(&base, dst)))
            }
            Op::Delete { src, dir } => ("DELETE", th.red, format!("{}{}{}", rel(&base, src), if *dir { "/" } else { "" }, if trash { "   (to Trash)" } else { "   (permanently!)" })),
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {tag:<7}"), Style::default().fg(col).add_modifier(Modifier::BOLD)),
            Span::styled(text, Style::default().fg(th.text)),
        ]));
    }
    let n = lines.len();
    let title = format!("Apply {n} change{}?  in {}", if n == 1 { "" } else { "s" }, tilde(&base));
    let w = lines.iter().map(|l| l.width()).max().unwrap_or(30).max(44).max(title.width() + 6) as u16 + 4;
    let h = (n as u16 + 4).min(area.height.saturating_sub(4));
    let r = centered(area, w, h);
    f.render_widget(Clear, r);
    let blk = frame(th, &title, if plan.deletes() > 0 { th.red } else { th.accent });
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    let [body, foot] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let max_scroll = n.saturating_sub(body.height as usize);
    let shown: Vec<Line> = lines.into_iter().skip((*scroll).min(max_scroll)).collect();
    f.render_widget(Paragraph::new(shown), body);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" y/⏎", Style::default().fg(th.green).add_modifier(Modifier::BOLD)),
            Span::styled(" apply   ", Style::default().fg(th.dim)),
            Span::styled("n/esc", Style::default().fg(th.red).add_modifier(Modifier::BOLD)),
            Span::styled(" keep editing", Style::default().fg(th.dim)),
            Span::styled(if max_scroll > 0 { "   j/k scroll" } else { "" }, Style::default().fg(th.dim)),
        ])),
        foot,
    );
}

fn draw_quit(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.th;
    let dirs = app.modified_dirs();
    let mut lines = vec![Line::styled(" Unsaved edits in:", Style::default().fg(th.text))];
    for d in dirs.iter().take(6) {
        lines.push(Line::styled(format!("   {}", tilde(d)), Style::default().fg(th.yellow)));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(" s", Style::default().fg(th.green).add_modifier(Modifier::BOLD)),
        Span::styled(" save & quit   ", Style::default().fg(th.dim)),
        Span::styled("d", Style::default().fg(th.red).add_modifier(Modifier::BOLD)),
        Span::styled(" discard & quit   ", Style::default().fg(th.dim)),
        Span::styled("esc", Style::default().fg(th.accent).add_modifier(Modifier::BOLD)),
        Span::styled(" cancel", Style::default().fg(th.dim)),
    ]));
    let w = lines.iter().map(|l| l.width()).max().unwrap_or(40) as u16 + 4;
    let r = centered(area, w, lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    let blk = frame(th, "Quit?", th.yellow);
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_report(f: &mut Frame, th: &Theme, area: Rect, title: &str, lines: &[String], error: bool, scroll: usize) {
    let col = if error { th.red } else { th.green };
    let ls: Vec<Line> = lines.iter().map(|l| Line::styled(format!(" {l}"), Style::default().fg(if l.starts_with("  ") { th.subtext } else { th.text }))).collect();
    let w = ls.iter().map(|l| l.width()).max().unwrap_or(30).max(60) as u16 + 4;
    let r = centered(area, w, ls.len() as u16 + 4);
    f.render_widget(Clear, r);
    let blk = frame(th, title, col);
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    let [body, foot] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    f.render_widget(Paragraph::new(ls.into_iter().skip(scroll).collect::<Vec<_>>()), body);
    f.render_widget(Paragraph::new(Line::styled(" any key to continue — your edits are still in the buffer", Style::default().fg(th.dim))), foot);
}

pub const HELP: &[(&str, &[(&str, &str)])] = &[
    ("Explorer — the listing is a text buffer", &[
        ("⏎ / dbl-click", "view file in oil · enter directory"),
        ("-  ⌫", "parent directory"),
        ("_  ~", "start directory · home"),
        ("^v ^x ^t", "open in split right / below / new tab"),
        ("^e", "edit in $EDITOR inside the popup, come back after"),
        ("gx", "open with system app"),
        ("gy", "copy absolute path"),
        ("g.  gs  gd", "hidden files · cycle sort · sizes → details → off"),
        ("^p", "toggle preview (^f/^b scroll it)"),
        ("^l  :e!", "refresh · discard edits here"),
        ("q  esc  ZZ  ZQ", "quit (asks if unsaved) · apply+quit · discard+quit"),
    ]),
    ("Viewer (⏎ on a file)", &[
        ("j k  ␣ b  ^d ^u  g G", "scroll · page · half page · top/bottom"),
        ("e", "edit in $EDITOR, back to the viewer after"),
        ("^v ^x ^t  o", "open in split / below / tab · system app"),
        ("y", "copy path"),
        ("q  esc  h  -", "back to the listing (or finder)"),
    ]),
    ("Editing = file operations (applied on :w)", &[
        ("cw ciw cin ...", "rename (cin = name stem, can = extension)"),
        ("o  O", "new line → create file; end with / for a dir"),
        ("a/b/c.txt", "nested paths create parents"),
        ("dd  {n}dd  V..d", "delete (to Trash by default)"),
        ("dd → - / ⏎ → p", "cut + paste in another dir = move"),
        ("yy → p", "duplicate line = copy"),
        ("u  ^r", "undo · redo"),
        (":w  ^s", "review & apply all pending changes"),
        (":touch a b  :mkdir d", "add entries quickly"),
        (":cd path  :sort mtime", "jump · sort name|mtime|size|ext"),
    ]),
    ("Vim motions", &[
        ("h j k l w b e 0 ^ $", "motions, with counts"),
        ("gg G {n}G", "first / last / nth line"),
        ("f F t T ;", "find char"),
        ("i a I A s S C D x r ~ J", "the usual"),
        ("/pattern n N", "search lines"),
    ]),
    ("Finder (⇥, ␣f files · ␣g grep · ␣d dirs · ␣r recent)", &[
        ("type", "fuzzy query ('exact ^prefix suffix$ !not)"),
        ("⇥ ⇧⇥", "cycle Files / Dirs / Grep / Recent"),
        ("⏎ ^v ^x ^t ^e", "open (default · split · below · tab · popup)"),
        ("^o", "reveal in explorer"),
        ("^space", "mark several files, ⏎ opens all"),
        ("^r", "root: cwd ⇄ repository ⇄ parent"),
        ("alt-h alt-i", "toggle hidden · ignored files"),
        ("^y", "copy path"),
        ("esc / ⌫ on empty", "back to explorer"),
    ]),
];

fn draw_help(f: &mut Frame, app: &App, area: Rect, scroll: usize) {
    let th = &app.th;
    let mut lines = Vec::new();
    for (sec, rows) in HELP {
        lines.push(Line::styled(format!(" {sec}"), Style::default().fg(th.accent).add_modifier(Modifier::BOLD)));
        for (k, v) in *rows {
            lines.push(Line::from(vec![Span::styled(format!("   {k:<24}"), Style::default().fg(th.yellow)), Span::styled(v.to_string(), Style::default().fg(th.text))]));
        }
        lines.push(Line::raw(""));
    }
    let r = centered(area, 78, area.height.saturating_sub(2));
    f.render_widget(Clear, r);
    let blk = frame(th, "herdr-oil keys  (j/k scroll · any key closes)", th.accent);
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    let max = lines.len().saturating_sub(inner.height as usize);
    f.render_widget(Paragraph::new(lines.into_iter().skip(scroll.min(max)).collect::<Vec<_>>()), inner);
}

trait SurfaceBg {
    fn surface_bg(&self) -> Color;
}
impl SurfaceBg for Theme {
    fn surface_bg(&self) -> Color {
        Color::Rgb(0x26, 0x26, 0x26)
    }
}
