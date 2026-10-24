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
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Terminal cells taken by `c` (wide CJK / emoji count double).
fn cw(c: char) -> usize {
    c.width().unwrap_or(0)
}

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
    if app.view == View::Explorer && matches!(app.overlay, Overlay::None) {
        draw_keyhint(f, app, main);
    }
    match &app.overlay {
        Overlay::None => {}
        Overlay::Confirm { .. } => draw_confirm(f, app, area),
        Overlay::Help { scroll, query, typing } => draw_help(f, app, area, *scroll, query, *typing),
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
    // entries leaving this directory, listed under the buffer
    let (mut gone, mut moved_out): (Vec<String>, Vec<String>) = (vec![], vec![]);
    if let Some(p) = &pending {
        for op in &p.ops {
            let here = |q: &Path| q.parent() == Some(app.cwd.as_path());
            let label = |q: &Path, dir: bool| format!("{}{}", q.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), if dir { "/" } else { "" });
            match op {
                Op::Delete { src, dir } if here(src) => gone.push(label(src, *dir)),
                Op::Move { src, dst, dir } if here(src) && !here(dst) => moved_out.push(label(src, *dir)),
                _ => {}
            }
        }
    }
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
                Op::Link { dst, .. } => {
                    c += 1;
                    tags.insert(plan::key(dst), '+');
                }
                Op::Retarget { path, .. } => {
                    r += 1;
                    tags.entry(plan::key(path)).or_insert('~');
                }
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
        "{}{}{}{} ",
        app.sort.label(),
        if app.store.reverse { " ↕" } else { "" },
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
    // footer row naming what this directory loses on :w
    let list_inner = if (gone.len() + moved_out.len()) > 0 && list_inner.height > 3 {
        let [rows, foot] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(list_inner);
        let mut fs = vec![Span::styled(" ", Style::default())];
        for (v, sym, verb, col) in [(&gone, "−", "deleted", th.red), (&moved_out, "→", "moved out", th.blue)] {
            if v.is_empty() {
                continue;
            }
            fs.push(Span::styled(format!("{sym} {} {verb}: ", v.len()), Style::default().fg(col).add_modifier(Modifier::BOLD)));
            fs.push(Span::styled(format!("{}   ", v.join(", ")), Style::default().fg(col).add_modifier(Modifier::DIM | Modifier::CROSSED_OUT)));
        }
        let used: usize = fs.iter().map(|s| s.content.width()).sum();
        let w = foot.width as usize;
        let mut line = Line::from(fs);
        if used > w {
            // keep it on one row
            let mut out = Vec::new();
            let mut left = w.saturating_sub(1);
            for sp in line.spans {
                let t: String = sp.content.chars().scan(0, |acc, c| { *acc += cw(c); (*acc <= left).then_some(c) }).collect();
                left = left.saturating_sub(t.width());
                out.push(Span::styled(t, sp.style));
            }
            out.push(Span::styled("…", Style::default().fg(th.dim)));
            line = Line::from(out);
        }
        f.render_widget(Paragraph::new(line).style(Style::default().bg(th.surface)), foot);
        rows
    } else {
        list_inner
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
        let (name_part, link_now) = plan::line_parts(&l.text, entry);
        let name_now = name_part.trim().trim_end_matches('/');
        // change marker, straight from the plan
        let untouched = entry.is_some_and(|e| e.name == name_now && e.path.parent() == Some(b.dir.as_path()) && link_now.map_or(true, |t| Some(t) == e.link_target.as_deref()));
        let (mark, mcol) = if !l.significant() || (untouched && original.contains(&l.id.unwrap_or(0))) {
            (" ", th.dim)
        } else {
            match plan::parse_name(name_part) {
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
        // virtual text: where a renamed / moved / copied line came from, a
        // symlink's target, and the details column on the right
        let mut after = Vec::new();
        let mut tail = Vec::new();
        if let Some(e) = entry {
            if matches!(mark, "~" | "→" | "⧉") {
                let from = if e.path.parent() == Some(b.dir.as_path()) { e.display() } else { tilde(&e.path) + if e.kind.is_dir() { "/" } else { "" } };
                let sym = if mark == "⧉" { "⧉" } else { "←" };
                after.push(Span::styled(format!("  {sym} {from}"), Style::default().fg(th.dim).add_modifier(Modifier::ITALIC)));
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
        let avail = w.saturating_sub(used + 1).max(4);
        let chars: Vec<char> = l.text.chars().collect();
        let is_cur = i == b.row;
        // horizontal scroll for the cursor line, in display cells
        let mut off = 0;
        if is_cur {
            let col = b.col.min(chars.len());
            let need = |o: usize| chars[o..col].iter().map(|c| cw(*c)).sum::<usize>() + chars.get(col).map(|c| cw(*c)).unwrap_or(1);
            while off < col && need(off) > avail {
                off += 1;
            }
            b.hscroll = off;
        }
        let mut shown: Vec<char> = Vec::new();
        let mut text_w = 0;
        for &c in chars.iter().skip(off) {
            if text_w + cw(c) > avail {
                break;
            }
            text_w += cw(c);
            shown.push(c);
        }
        let in_vis = vis.is_some_and(|(a, z)| i >= a && i <= z);
        let row_bg = if in_vis {
            Some(th.surface)
        } else if is_cur && matches!(mode, Mode::Normal | Mode::Cmd { .. }) {
            Some(th.selection)
        } else {
            None
        };
        if empty_dir && i == 0 && !matches!(mode, Mode::Insert) {
            if matches!(mode, Mode::Normal) {
                spans.push(Span::styled(" ", base.add_modifier(Modifier::REVERSED)));
            }
            spans.push(Span::styled("(empty — o to add a file, name/ for a dir)", Style::default().fg(th.dim).add_modifier(Modifier::ITALIC)));
        } else {
            // ` -> target` of a symlink line in teal; the normal-mode cursor reversed
            let arrow = link_now.map(|_| name_part.chars().count());
            let link_style = Style::default().fg(th.teal).add_modifier(Modifier::DIM);
            let cursor = (is_cur && matches!(mode, Mode::Normal)).then(|| b.col.saturating_sub(off));
            let mut cells: Vec<(char, Style)> = shown
                .iter()
                .enumerate()
                .map(|(k, &c)| {
                    let st = if arrow.is_some_and(|a| off + k >= a) { link_style } else { base };
                    (c, if cursor == Some(k) { st.add_modifier(Modifier::REVERSED) } else { st })
                })
                .collect();
            if cursor.is_some_and(|c| c >= shown.len()) {
                cells.push((' ', base.add_modifier(Modifier::REVERSED)));
            }
            let mut run = String::new();
            let mut st = base;
            for (c, cst) in cells {
                if cst != st && !run.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut run), st));
                }
                st = cst;
                run.push(c);
            }
            spans.push(Span::styled(run, st));
        }
        if is_cur && matches!(mode, Mode::Insert) {
            let cx: usize = chars[off..b.col.min(chars.len())].iter().map(|c| cw(*c)).sum();
            let x = list_inner.x + used as u16 + cx as u16;
            cursor_pos = Some(Position::new(x.min(list_inner.right().saturating_sub(1)), list_inner.y + (i - b.scroll) as u16));
        }
        let text_w = text_w.max(if is_cur { 1 } else { 0 });
        let tail_w: usize = tail.iter().map(|s| s.content.width()).sum();
        let mut room = w.saturating_sub(used + text_w + tail_w);
        for sp in after {
            let aw = sp.content.width();
            if aw < room {
                room -= aw;
                spans.push(sp);
            }
        }
        if !tail.is_empty() && used + text_w + tail_w < w {
            spans.push(Span::raw(" ".repeat(room)));
            spans.extend(tail);
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
        draw_preview(f, app, pr, target.as_deref(), None, false);
    }
}

fn draw_preview(f: &mut Frame, app: &mut App, area: Rect, path: Option<&Path>, line: Option<usize>, diff: bool) {
    let th = app.th.clone();
    let [title, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let Some(path) = path else {
        f.render_widget(Paragraph::new(Line::styled("  no preview", Style::default().fg(th.dim))), body);
        return;
    };
    let meta = std::fs::symlink_metadata(path).ok();
    if meta.is_none() && !diff {
        f.render_widget(Paragraph::new(Line::styled("  no preview", Style::default().fg(th.dim))), body);
        return;
    }
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
    let c = app.preview.get(path, line, body.width, &th, hidden, crate::preview::PREVIEW_LINES, diff);
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
    let c = app.viewer_cache.get(&v.path, v.line, body.width.saturating_sub(1), &th, hidden, crate::preview::VIEWER_LINES, false);
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
    let mut pos = if v.total == 0 { String::new() } else { format!("{}–{} of {} ", top + 1, (top + h).min(v.total), v.total) };
    if !v.search.is_empty() && !v.hits.is_empty() {
        let i = v.at.and_then(|a| v.hits.iter().position(|&x| x == a)).map(|i| i + 1).unwrap_or(0);
        pos = format!("/{} {i}/{}   {pos}", v.search, v.hits.len());
    }
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
    // search hits: matches highlighted, a bar in the gutter, current hit's row tinted
    let mut gutter = Vec::new();
    let lines: Vec<Line> = c
        .lines
        .iter()
        .enumerate()
        .skip(top)
        .take(h)
        .map(|(i, l)| {
            let hit = !v.search.is_empty() && v.hits.binary_search(&i).is_ok();
            gutter.push(Line::styled(if hit { "▌" } else { " " }, Style::default().fg(if v.at == Some(i) { th.yellow } else { th.peach })));
            if !hit {
                return l.clone();
            }
            let l = mark_matches(l, &v.search, Style::default().fg(Color::Black).bg(th.yellow));
            if v.at == Some(i) { l.style(Style::default().bg(th.selection)) } else { l }
        })
        .collect();
    app.list_area = body;
    f.render_widget(Paragraph::new(gutter), Rect { width: 1.min(body.width), ..body });
    f.render_widget(Paragraph::new(lines), Rect { x: body.x + 1, width: body.width.saturating_sub(1), ..body });
}

/// Restyle every (smart-case) occurrence of `pat` inside a styled line.
fn mark_matches(l: &Line<'static>, pat: &str, hl: Style) -> Line<'static> {
    let smart = pat.chars().any(|c| c.is_uppercase());
    let fold = |c: char| if smart { c } else { c.to_lowercase().next().unwrap_or(c) };
    let cells: Vec<(char, Style)> = l.spans.iter().flat_map(|s| s.content.chars().map(move |c| (c, s.style))).collect();
    let p: Vec<char> = pat.chars().map(fold).collect();
    let mut on = vec![false; cells.len()];
    let mut i = 0;
    while !p.is_empty() && i + p.len() <= cells.len() {
        if cells[i..i + p.len()].iter().zip(&p).all(|((c, _), q)| fold(*c) == *q) {
            on[i..i + p.len()].iter_mut().for_each(|x| *x = true);
            i += p.len();
        } else {
            i += 1;
        }
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (k, (c, st)) in cells.into_iter().enumerate() {
        let st = if on[k] { st.patch(hl) } else { st };
        match spans.last_mut() {
            Some(last) if last.style == st => last.content.to_mut().push(c),
            _ => spans.push(Span::styled(c.to_string(), st)),
        }
    }
    Line::from(spans).style(l.style)
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
    let transfer = app.transfer.as_ref().map(|t| (t.copy, t.count));
    let Some(fd) = app.finder.as_mut() else { return };

    // tabs
    let mut ts = vec![Span::raw(" ")];
    for (k, label, ic) in [
        (FinderKind::Files, "Files", "\u{f0214}"),
        (FinderKind::Dirs, "Dirs", "\u{f024b}"),
        (FinderKind::Grep, "Grep", "\u{f0349}"),
        (FinderKind::Recent, "Recent", "\u{f02da}"),
        (FinderKind::Git, "Git", "\u{e702}"),
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
        _ if transfer.is_some() => "pick the destination folder (a file means its folder)…",
        FinderKind::Grep => "search file contents (ripgrep, smart-case)  · foo -- *.rs -t py",
        FinderKind::Dirs => "fuzzy-find directories…",
        FinderKind::Recent => "recently opened files…",
        FinderKind::Git => "changed & untracked files (git status) — preview shows the diff…",
        FinderKind::Files => "fuzzy-find files  ('exact ^prefix suffix$ !not)",
    };
    let mut ps = vec![Span::styled(" \u{f0349} ", Style::default().fg(th.accent).add_modifier(Modifier::BOLD))];
    let mut lead_w = 3;
    if let Some((copy, n)) = transfer {
        let label = format!(" {} {n} → ", if copy { "COPY" } else { "MOVE" });
        lead_w += label.width() + 1;
        ps.push(Span::styled(label, Style::default().fg(Color::Black).bg(if copy { th.blue } else { th.peach }).add_modifier(Modifier::BOLD)));
        ps.push(Span::raw(" "));
    }
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
        f.set_cursor_position(Position::new(prompt.x + lead_w as u16 + qx as u16, prompt.y));
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
    let query = crate::finder::grep_query(&fd.query).0;
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
        sp.push(Span::styled(if marked.iter().any(|m| m.path == it.abs && m.line == it.line) { "●" } else { " " }, Style::default().fg(th.peach)));
        if let Some(g) = it.git {
            let gcol = match g {
                'M' | 'R' => th.yellow,
                'A' => th.green,
                '?' => th.teal,
                _ => th.red,
            };
            sp.push(Span::styled(format!("{g} "), Style::default().fg(gcol).add_modifier(Modifier::BOLD)));
        }
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
            FinderKind::Git if app.finder.as_ref().is_some_and(|f| f.git.is_none()) => "not in a git repository (or it defines its own filters)",
            FinderKind::Git if query.is_empty() => "working tree clean",
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
            Some(it) => draw_preview(f, app, inner, Some(&it.abs), it.line, it.git.is_some_and(|c| c != '?')),
            None => draw_preview(f, app, inner, None, None, false),
        }
    }
}

// ── status line ─────────────────────────────────────────────────────────────

/// Which-key style menu: after a prefix key (`g`, `␣`, `Z`) list what can follow.
pub const KEYHINTS: &[(&str, &[(&str, &str)])] = &[
    ("g", &[
        ("g", "first line"),
        (".", "show/hide hidden files"),
        ("s", "cycle sort"),
        ("S", "reverse sort"),
        ("d", "sizes → details → off"),
        ("p", "toggle preview"),
        ("x", "open with system app"),
        ("y", "copy relative path"),
        ("Y", "copy absolute path"),
        ("n", "copy file name"),
        ("i", "send path to origin pane"),
        ("e", "edit listing in $EDITOR"),
        ("f", "find files"),
        ("/", "grep"),
        ("?", "all keys"),
    ]),
    (" ", &[("f", "find files"), ("g", "grep"), ("d", "find folders"), ("r", "recent files"), ("s", "git changes"), ("␣", "find files")]),
    ("Z", &[("Z", "apply and quit"), ("Q", "quit, discard edits")]),
];

fn draw_keyhint(f: &mut Frame, app: &App, area: Rect) {
    let pend = app.vim.pending_str();
    let key: String = pend.trim_start_matches(|c: char| c.is_ascii_digit()).to_string();
    // marks: m{a-z} sets, '{a-z} jumps — list what is there
    let marks: Vec<(String, String)> = app.marks.iter().map(|(k, p)| (k.to_string(), tilde(p))).collect();
    let mark_rows: Vec<(&str, &str)> = match key.as_str() {
        "'" | "`" if marks.is_empty() => vec![("", "no marks yet — m{a-z} marks this folder")],
        "'" | "`" => marks.iter().map(|(k, p)| (k.as_str(), p.as_str())).collect(),
        "m" => std::iter::once(("a-z", "mark this folder")).chain(marks.iter().map(|(k, p)| (k.as_str(), p.as_str()))).collect(),
        _ => vec![],
    };
    let base: &[(&str, &str)] = match KEYHINTS.iter().find(|(k, _)| *k == key) {
        Some((_, b)) => b,
        None if !mark_rows.is_empty() => &mark_rows,
        None => return,
    };
    let th = &app.th;
    // gm / gc only exist while something is yanked or cut
    let n = app.vim.reg_entries();
    let (mv, cp) = (format!("move {n} yanked → pick folder"), format!("copy {n} yanked → pick folder"));
    let mut rows: Vec<(&str, &str)> = Vec::new();
    if key == "g" && n > 0 {
        rows.push(("m", mv.as_str()));
        if !app.vim.reg_cut {
            rows.push(("c", cp.as_str()));
        }
    }
    rows.extend(base.iter().copied());
    let label = if key == " " { "␣".to_string() } else { key.clone() };
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!(" {label}{k} "), Style::default().fg(th.yellow).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {v} "), Style::default().fg(th.text)),
            ])
        })
        .collect();
    let w = lines.iter().map(|l| l.width()).max().unwrap_or(20) as u16 + 2;
    let h = lines.len() as u16 + 2;
    let r = Rect {
        x: area.right().saturating_sub(w + 1),
        y: area.bottom().saturating_sub(h),
        width: w.min(area.width),
        height: h.min(area.height),
    };
    f.render_widget(Clear, r);
    let title = format!("{label}…");
    let blk = frame(th, &title, th.accent);
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.th;
    let typing = match (app.view, &app.vim.mode) {
        (View::Explorer, Mode::Cmd { kind, text, cur, .. }) => Some((*kind, text.clone(), *cur)),
        (View::Viewer, _) => app.viewer.as_ref().and_then(|v| v.typing.clone()).map(|t| ('/', t.clone(), t.chars().count())),
        _ => None,
    };
    if let Some((kind, text, cur)) = typing {
        let mut sp = vec![Span::styled(kind.to_string(), Style::default().fg(th.accent).add_modifier(Modifier::BOLD)), Span::styled(text.clone(), Style::default().fg(th.text))];
        // completions / live :s match count, right-aligned
        if let Some((m, k, _)) = &app.msg {
            let used = 1 + text.width();
            let mw = m.width();
            if used + mw + 3 < area.width as usize {
                sp.push(Span::raw(" ".repeat(area.width as usize - used - mw - 1)));
                sp.push(Span::styled(m.clone(), Style::default().fg(if *k == MsgKind::Err { th.red } else { th.dim })));
            }
        }
        f.render_widget(Paragraph::new(Line::from(sp)), area);
        let x: usize = text.chars().take(cur).map(|c| c.to_string().width()).sum();
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
        (View::Viewer, _) => &[("j/k ␣ b", "scroll"), ("/ n N", "search"), ("e", "edit here"), ("p", "send"), ("y", "copy path"), ("q", "back")],
        (View::Finder, _) if app.transfer.is_some() => &[("⏎", "here"), ("^n", "new folder from query"), ("⇥", "files/dirs"), ("^r", "root"), ("esc", "cancel")],
        (View::Finder, _) => &[("⏎", "open"), ("^v", "split"), ("^o", "reveal"), ("^s", "send"), ("⇥", "mode"), ("^r", "root"), ("esc", "back")],
        (_, Mode::Insert) => &[("esc", "normal"), ("⏎", "new line"), ("^s", "save")],
        (_, Mode::Visual { .. }) => &[("d", "cut"), ("y", "yank"), ("c", "change"), ("I A", "prefix/suffix"), (":", "s/…/…/"), ("esc", "cancel")],
        _ if app.vim.reg_entries() > 0 && app.vim.reg_cut => &[("⏎", "open"), ("p", "paste"), ("gm", "move cut to…"), (":w", "apply"), ("⇥", "find"), ("g?", "help"), ("esc", "close")],
        _ if app.vim.reg_entries() > 0 => &[("⏎", "open"), ("p", "paste"), ("gm", "move yanked to…"), ("gc", "copy yanked to…"), (":w", "apply"), ("g?", "help"), ("esc", "close")],
        _ => &[("⏎", "open"), ("-", "up"), ("H L", "back/fwd"), (":w", "apply"), ("⇥", "find"), ("␣/", "grep"), ("g?", "help"), ("esc", "close")],
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
        let relink = match op {
            Op::Retarget { path, .. } => Some(path.as_path()),
            _ => None,
        };
        for p in [op.src(), op.dst(), relink].into_iter().flatten() {
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

/// Old → new with only the differing middle highlighted: common prefix and
/// suffix stay plain, removed chars red/struck, added chars green.
fn diff_spans(th: &Theme, a: &str, b: &str) -> Vec<Span<'static>> {
    let (ac, bc): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let pre = ac.iter().zip(&bc).take_while(|(x, y)| x == y).count();
    let max_suf = ac.len().min(bc.len()) - pre;
    let suf = ac.iter().rev().zip(bc.iter().rev()).take(max_suf).take_while(|(x, y)| x == y).count();
    let s = |v: &[char]| v.iter().collect::<String>();
    let plain = Style::default().fg(th.subtext);
    let del = Style::default().fg(th.red).add_modifier(Modifier::CROSSED_OUT);
    let add = Style::default().fg(th.green).add_modifier(Modifier::BOLD);
    let mut out = Vec::new();
    for (v, hl) in [(&ac, del), (&bc, add)] {
        if !out.is_empty() {
            out.push(Span::styled("  →  ", Style::default().fg(th.dim)));
        }
        out.push(Span::styled(s(&v[..pre]), plain));
        out.push(Span::styled(s(&v[pre..v.len() - suf]), hl));
        out.push(Span::styled(s(&v[v.len() - suf..]), plain));
    }
    out
}

fn draw_confirm(f: &mut Frame, app: &App, area: Rect) {
    let Overlay::Confirm { plan, scroll, undo, .. } = &app.overlay else { return };
    let th = &app.th;
    let trash = *undo || matches!(app.cfg.delete, crate::config::DeleteMethod::Trash);
    let base = common_base(plan);
    let mut lines = Vec::new();
    let mut counts: Vec<(&str, usize, Color)> = Vec::new();
    for op in &plan.ops {
        let slash = |d: bool| if d { "/" } else { "" };
        let (tag, col, body): (&str, Color, Vec<Span>) = match op {
            Op::Create { dst, dir } => ("CREATE", th.green, vec![Span::styled(format!("{}{}", rel(&base, dst), slash(*dir)), Style::default().fg(th.green))]),
            Op::Move { src, dst, dir } => {
                let same_dir = src.parent() == dst.parent();
                let (a, b) = (format!("{}{}", rel(&base, src), slash(*dir)), format!("{}{}", rel(&base, dst), slash(*dir)));
                (if same_dir { "RENAME" } else { "MOVE" }, if same_dir { th.yellow } else { th.blue }, diff_spans(th, &a, &b))
            }
            Op::Copy { src, dst, dir } => ("COPY", th.blue, diff_spans(th, &format!("{}{}", rel(&base, src), slash(*dir)), &format!("{}{}", rel(&base, dst), slash(*dir)))),
            Op::Link { dst, target } => ("SYMLINK", th.teal, vec![Span::styled(rel(&base, dst), Style::default().fg(th.green)), Span::styled(format!("  ->  {target}"), Style::default().fg(th.teal))]),
            Op::Retarget { path, target } => {
                let old = std::fs::read_link(path).map(|t| t.display().to_string()).unwrap_or_default();
                let mut v = vec![Span::styled(format!("{}  ", rel(&base, path)), Style::default().fg(th.text))];
                v.extend(diff_spans(th, &old, target));
                ("RELINK", th.teal, v)
            }
            Op::Delete { src, dir } => (
                "DELETE",
                th.red,
                vec![
                    Span::styled(format!("{}{}", rel(&base, src), slash(*dir)), Style::default().fg(th.text)),
                    Span::styled(if trash { "   (to Trash)" } else { "   (permanently!)" }, Style::default().fg(if trash { th.dim } else { th.red })),
                ],
            ),
        };
        match counts.iter_mut().find(|(t, _, _)| *t == tag) {
            Some(c) => c.1 += 1,
            None => counts.push((tag, 1, col)),
        }
        let mut sp = vec![Span::styled(format!(" {tag:<7}"), Style::default().fg(col).add_modifier(Modifier::BOLD))];
        sp.extend(body);
        lines.push(Line::from(sp));
    }
    // one-line summary on top: "2 rename · 1 delete"
    let mut sum = vec![Span::raw(" ")];
    for (k, (tag, c, col)) in counts.iter().enumerate() {
        if k > 0 {
            sum.push(Span::styled(" · ", Style::default().fg(th.dim)));
        }
        sum.push(Span::styled(format!("{c} {}", tag.to_lowercase()), Style::default().fg(*col).add_modifier(Modifier::BOLD)));
    }
    lines.insert(0, Line::from(sum));
    lines.insert(1, Line::raw(""));
    let n = plan.ops.len();
    let title = format!("{} {n} change{}?  in {}", if *undo { "Undo last apply:" } else { "Apply" }, if n == 1 { "" } else { "s" }, tilde(&base));
    let w = lines.iter().map(|l| l.width()).max().unwrap_or(30).max(44).max(title.width() + 6) as u16 + 4;
    let h = (lines.len() as u16 + 4).min(area.height.saturating_sub(4));
    let r = centered(area, w, h);
    f.render_widget(Clear, r);
    let blk = frame(th, &title, if plan.deletes() > 0 { th.red } else { th.accent });
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    let [body, foot] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let max_scroll = lines.len().saturating_sub(body.height as usize);
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
        ("H  L", "back · forward through visited folders"),
        ("m{a-z}  '{a-z}  :marks", "mark this folder · jump to a mark · list"),
        ("gx", "open with system app"),
        ("gy  gY  gn", "copy relative path · absolute path · name"),
        ("gi  :'<,'>send", "type path(s) into the origin pane (@path for agents)"),
        ("ge", "edit this listing in $EDITOR (/001 tags keep identity)"),
        ("g.  gs  gS  gd", "hidden · cycle sort · reverse · sizes → details → off"),
        ("^p", "toggle preview (^f/^b scroll it)"),
        ("^l  :e!", "refresh · discard edits here"),
        ("g?  then /", "this help · type to filter it"),
        ("q  esc  ZZ  ZQ", "quit (asks if unsaved) · apply+quit · discard+quit"),
    ]),
    ("Viewer (⏎ on a file)", &[
        ("j k  ␣ b  ^d ^u  g G", "scroll · page · half page · top/bottom"),
        ("/pattern  n N", "search the file · next / previous hit"),
        ("p", "send path (:line of the hit) to the origin pane"),
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
        ("yy/dd → gm  yy → gc", "move / copy yanked entries to a fuzzy-picked folder"),
        ("yy → p", "duplicate line = copy"),
        ("u  ^r  .", "undo · redo · repeat last change on this line"),
        (":%s/old/new/g", "rename by regex (live preview, $1 or \\1 groups)"),
        ("V.. :s  I  A", "same on a selection · prefix · suffix the names"),
        (":prefix x  :suffix x", "prepend · append before the extension"),
        ("name -> target", "symlink: edit the target to relink, new line creates"),
        ("U  :undo-apply", "undo the last apply (U again redoes)"),
        (":chmod +x  :%chmod 644", "change permissions (applies at once)"),
        (":w  ^s", "review & apply all pending changes"),
        (":touch a b  :mkdir d", "add entries quickly"),
        (":cd path ⇥", "jump, ⇥ completes paths and commands"),
        (":sort mtime  :sort!", "sort name|mtime|size|ext · reverse"),
    ]),
    ("Vim motions", &[
        ("h j k l w b e 0 ^ $", "motions, with counts"),
        ("gg G {n}G", "first / last / nth line"),
        ("f F t T ; ,", "find char · repeat · reverse"),
        ("i a I A s S C D x r ~ J", "the usual"),
        ("/pattern n N", "search lines"),
    ]),
    ("Finder (⇥, ␣f files · ␣g grep · ␣d dirs · ␣r recent · ␣s git)", &[
        ("type", "fuzzy query ('exact ^prefix suffix$ !not)"),
        ("foo -- *.rs  -t py", "grep: only these globs / file types"),
        ("⇥ ⇧⇥", "cycle Files / Dirs / Grep / Recent / Git"),
        ("^s", "send selection / marked (with :line) to the origin pane"),
        ("⏎ ^v ^x ^t ^e", "open (default · split · below · tab · popup)"),
        ("^o", "reveal in explorer"),
        ("^space", "mark files / grep hits (line kept), ⏎ opens all"),
        ("^r", "root: cwd ⇄ repository ⇄ parent"),
        ("alt-h alt-i", "toggle hidden · ignored files"),
        ("^y", "copy path"),
        ("esc / ⌫ on empty", "back to explorer"),
    ]),
];

/// Help rows matching `query` (case-insensitive, in key, text or section
/// title); a matching section keeps all its rows.
pub fn help_rows(query: &str) -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    let q = query.to_lowercase();
    let hit = |t: &str| t.to_lowercase().contains(&q);
    HELP.iter()
        .filter_map(|(sec, rows)| {
            let rows: Vec<_> = rows.iter().copied().filter(|(k, v)| q.is_empty() || hit(sec) || hit(k) || hit(v)).collect();
            (!rows.is_empty()).then_some((*sec, rows))
        })
        .collect()
}

fn draw_help(f: &mut Frame, app: &App, area: Rect, scroll: usize, query: &str, typing: bool) {
    let th = &app.th;
    let mut lines = Vec::new();
    if typing || !query.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" /", Style::default().fg(th.accent)),
            Span::styled(query.to_string(), Style::default().fg(th.text)),
            Span::styled(if typing { "▏" } else { "" }, Style::default().fg(th.accent)),
        ]));
        lines.push(Line::raw(""));
    }
    let sections = help_rows(query);
    if sections.is_empty() {
        lines.push(Line::styled("   no key matches", Style::default().fg(th.dim)));
    }
    for (sec, rows) in sections {
        lines.push(Line::styled(format!(" {sec}"), Style::default().fg(th.accent).add_modifier(Modifier::BOLD)));
        for (k, v) in rows {
            lines.push(Line::from(vec![Span::styled(format!("   {k:<24}"), Style::default().fg(th.yellow)), Span::styled(v.to_string(), Style::default().fg(th.text))]));
        }
        lines.push(Line::raw(""));
    }
    let r = centered(area, 78, area.height.saturating_sub(2));
    f.render_widget(Clear, r);
    let title = if typing { "herdr-oil keys  (type to filter · ⏎ done · esc clear)" } else { "herdr-oil keys  (/ filter · j/k scroll · any key closes)" };
    let blk = frame(th, title, th.accent);
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
