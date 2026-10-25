//! Rendering: explorer (editable listing + optional preview), finder
//! (tabs, prompt, results, preview), overlays and the status line.

use crate::app::{App, MsgKind, Overlay, View};
use crate::entry::{self, IconColor, Kind};
use crate::finder::tilde;
use crate::plan::{self, Op};
use std::collections::HashMap;
use crate::theme::Theme;
use crate::preview::Show;
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

/// Status letter colour: green staged only, yellow (red for deletions)
/// unstaged only, peach both, teal untracked, red conflicts, dim ignored.
pub fn git_color(th: &Theme, g: entry::Git) -> Color {
    match g.letter {
        '?' => th.teal,
        '!' => th.dim,
        'U' => th.red,
        _ if g.partial() => th.peach,
        _ if g.staged => th.green,
        'D' => th.red,
        _ => th.yellow,
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
        Overlay::Ask { title, lines, .. } => draw_ask(f, &app.th, area, title, lines),
        Overlay::History { sel } => draw_history(f, app, area, *sel),
        Overlay::Conflicts { lines, scroll } => draw_conflicts(f, &app.th, area, lines, *scroll),
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
    let pending = app.pending_plan();
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
        "{}{}{}{}{} ",
        app.sort.label(),
        if app.store.reverse { " ↕" } else { "" },
        if app.hidden { " · hidden" } else { "" },
        match app.details {
            2 => " · details",
            3 => " · meta",
            _ => "",
        },
        if app.git_only { " · changed only" } else { "" }
    );
    let branch = app.buf().branch.as_ref().map(|b| format!("\u{e725} {}  ", b.badge())).unwrap_or_default();
    let dirty = if app.buf().modified() { " [+]" } else { "" };
    let sum_w: usize = summary.iter().map(|s| s.content.width()).sum();
    let budget = (head.width as usize).saturating_sub(3 + sum_w + right.width() + branch.width() + dirty.width() + 1).max(8);
    // path segments, each clickable; leading ones give way to "…/" when narrow
    let mut segs: Vec<(String, std::path::PathBuf)> = Vec::new();
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty()).map(std::path::PathBuf::from);
    let mut acc = std::path::PathBuf::new();
    for c in app.cwd.components() {
        acc.push(c.as_os_str());
        if home.as_ref().is_some_and(|h| acc.starts_with(h) && acc != *h) || home.as_ref() == Some(&acc) {
            if home.as_ref() == Some(&acc) {
                segs.clear();
                segs.push(("~/".into(), acc.clone()));
            } else {
                segs.push((format!("{}/", c.as_os_str().to_string_lossy()), acc.clone()));
            }
            continue;
        }
        let t = match c {
            std::path::Component::RootDir => "/".to_string(),
            _ => format!("{}/", c.as_os_str().to_string_lossy()),
        };
        segs.push((t, acc.clone()));
    }
    let mut cut = 0;
    while cut + 1 < segs.len() && segs[cut..].iter().map(|(t, _)| t.width()).sum::<usize>() + 2 > budget {
        cut += 1;
    }
    let mut hs = vec![Span::styled(" \u{f07c} ", Style::default().fg(th.accent))];
    let mut x = head.x + 3;
    app.crumbs.clear();
    app.head_y = head.y;
    if cut > 0 {
        hs.push(Span::styled("…/", Style::default().fg(th.dim)));
        x += 2;
    }
    let n = segs.len();
    for (i, (t, p)) in segs.into_iter().enumerate().skip(cut) {
        let last = i + 1 == n;
        let w = t.width() as u16;
        app.crumbs.push((x, x + w, p));
        x += w;
        let st = if last { Style::default().fg(th.text).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.subtext) };
        hs.push(Span::styled(if last && t.len() > 1 { t.trim_end_matches('/').to_string() + "/" } else { t }, st));
    }
    hs.push(Span::styled(dirty, Style::default().fg(th.yellow).add_modifier(Modifier::BOLD)));
    hs.extend(summary);
    let used: usize = hs.iter().map(|s| s.content.width()).sum();
    let pad = (head.width as usize).saturating_sub(used + right.width() + branch.width());
    hs.push(Span::raw(" ".repeat(pad)));
    hs.push(Span::styled(branch, Style::default().fg(th.accent)));
    hs.push(Span::styled(right, Style::default().fg(th.dim)));
    f.render_widget(Paragraph::new(Line::from(hs)), head);

    // gw: the parent folder as a narrow column on the left
    let body = if app.show_parent && body.width >= 90 {
        let [pc, rest] = Layout::horizontal([Constraint::Percentage(15), Constraint::Min(1)]).areas(body);
        draw_parent(f, app, pc);
        rest
    } else {
        body
    };
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
    // `/` matches (while typing, and after until :noh), lines a `:g/…/d`
    // being typed would delete, and what a live `:s` changes
    let search = match &mode {
        Mode::Cmd { kind: '/', text, .. } => text.clone(),
        _ => app.vim.search.clone(),
    };
    let doomed: std::collections::HashSet<usize> = app.vim.doomed.iter().copied().collect();
    let sub_base = app.vim.preview_base().filter(|base| base.len() == b.lines.len());
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
        let mut conflict = None;
        let doomed = doomed.contains(&i);
        let (mark, mcol) = if doomed {
            ("−", th.red)
        } else if !l.significant() || (untouched && original.contains(&l.id.unwrap_or(0))) {
            (" ", th.dim)
        } else {
            match plan::parse_name(name_part) {
                Err(_) => ("!", th.red),
                Ok(None) => (" ", th.dim),
                Ok(Some((rel, _))) => {
                    let k = plan::key(&plan::normalize(&b.dir.join(rel)));
                    // a clash `:w` would refuse: flag it while editing
                    conflict = pending.as_ref().and_then(|p| p.conflicts.get(&k));
                    match tags.get(&k) {
                        Some(_) if conflict.is_some() => ("!", th.red),
                        Some('+') => ("+", th.green),
                        Some('~') => ("~", th.yellow),
                        Some('→') => ("→", th.blue),
                        Some('⧉') => ("⧉", th.blue),
                        _ if l.id.is_some() && entry.is_none() => ("?", th.red),
                        _ => (" ", th.dim),
                    }
                }
            }
        };
        // keep the badge on a line being renamed (not on copies or lines moved in)
        let gitc = entry
            .filter(|e| e.path.parent() == Some(b.dir.as_path()) && (e.name == name_now || mark == "~"))
            .and_then(|e| b.git.get(&e.name).copied());
        let gcol = gitc.map(|g| git_color(&th, g)).unwrap_or(th.dim);
        let mut spans = vec![
            Span::styled(mark.to_string(), Style::default().fg(mcol).add_modifier(Modifier::BOLD)),
            Span::styled(gitc.map(|g| g.letter.to_string()).unwrap_or_else(|| " ".into()), Style::default().fg(gcol)),
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
        let base = if gitc.is_some_and(|g| g.letter == '!') { base.add_modifier(Modifier::DIM) } else { base };
        // virtual text: where a renamed / moved / copied line came from, a
        // symlink's target, and the details column on the right
        let mut after = Vec::new();
        let mut tail = Vec::new();
        if let Some(why) = conflict {
            after.push(Span::styled(format!("  ✗ {why}"), Style::default().fg(th.red).add_modifier(Modifier::ITALIC)));
        }
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
            if details == 3 && w > 60 {
                let x = if entry::has_xattrs(&e.path) { "@" } else { " " };
                tail.push(Span::styled(format!("  {}  ", entry::owner(e.uid, e.gid)), Style::default().fg(th.subtext)));
                tail.push(Span::styled(format!("{}{x}", entry::perms(e.mode, e.kind)), Style::default().fg(th.dim)));
                if entry::broken_link(e) {
                    tail.push(Span::styled("  ⚠ broken link", Style::default().fg(th.red)));
                } else {
                    tail.push(Span::styled(format!("{size:>9}"), Style::default().fg(scol)));
                }
            } else if details == 2 && w > 50 {
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
            let base = if doomed { Style::default().fg(th.red).add_modifier(Modifier::CROSSED_OUT | Modifier::DIM) } else { base };
            let found = match_ranges(&chars, &search);
            let changed = sub_base.and_then(|bs| changed_range(&bs[i].text, &l.text));
            let hl = Style::default().fg(Color::Black).bg(th.yellow);
            let subst = Style::default().fg(th.green).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            let mut cells: Vec<(char, Style)> = shown
                .iter()
                .enumerate()
                .map(|(k, &c)| {
                    let at = off + k;
                    let mut st = if arrow.is_some_and(|a| at >= a) { link_style } else { base };
                    if changed.is_some_and(|(a, z)| at >= a && at < z) {
                        st = st.patch(subst);
                    }
                    if found.iter().any(|&(a, z)| at >= a && at < z) {
                        st = st.patch(hl);
                    }
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
        // the entry a line came from, even while it is being renamed
        let target = b.cur_source_entry(store).map(|e| e.path.clone());
        draw_preview(f, app, pr, target.as_deref(), None, Show::File);
    }
}

/// Char ranges of smart-case occurrences of `pat` in `chars`.
fn match_ranges(chars: &[char], pat: &str) -> Vec<(usize, usize)> {
    if pat.is_empty() {
        return vec![];
    }
    let smart = pat.chars().any(|c| c.is_uppercase());
    let fold = |c: char| if smart { c } else { c.to_lowercase().next().unwrap_or(c) };
    let p: Vec<char> = pat.chars().map(fold).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + p.len() <= chars.len() {
        if chars[i..i + p.len()].iter().zip(&p).all(|(c, q)| fold(*c) == *q) {
            out.push((i, i + p.len()));
            i += p.len();
        } else {
            i += 1;
        }
    }
    out
}

/// Chars of `new` that differ from `old` (between the common prefix and suffix).
fn changed_range(old: &str, new: &str) -> Option<(usize, usize)> {
    if old == new {
        return None;
    }
    let (a, b): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let max_suf = a.len().min(b.len()) - pre;
    let suf = a.iter().rev().zip(b.iter().rev()).take(max_suf).take_while(|(x, y)| x == y).count();
    let z = b.len() - suf;
    // a pure deletion leaves nothing to colour: mark the char after the cut
    Some(if z > pre { (pre, z) } else { (pre, (pre + 1).min(b.len())) })
}

/// `gw`: the parent folder's entries, the current folder highlighted.
fn draw_parent(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.th.clone();
    let Some(parent) = app.cwd.parent().map(Path::to_path_buf) else { return };
    let fresh = app.parent_cache.as_ref().is_some_and(|(p, _)| *p == parent);
    if !fresh {
        let mut v: Vec<(String, bool)> = std::fs::read_dir(&parent)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| (e.file_name().to_string_lossy().into_owned(), e.path().is_dir())).filter(|(n, _)| app.hidden || !n.starts_with('.')).collect())
            .unwrap_or_default();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| entry::natural_cmp(&a.0, &b.0)));
        app.parent_cache = Some((parent.clone(), v));
    }
    let here = app.cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let Some((_, items)) = &app.parent_cache else { return };
    let blk = Block::default().borders(Borders::RIGHT).border_style(Style::default().fg(th.dim));
    let inner = blk.inner(area);
    f.render_widget(blk, area);
    let h = inner.height as usize;
    let at = items.iter().position(|(n, _)| *n == here).unwrap_or(0);
    let skip = at.saturating_sub(h / 2);
    let w = inner.width as usize;
    let lines: Vec<Line> = items
        .iter()
        .skip(skip)
        .take(h)
        .map(|(n, dir)| {
            let cur = *n == here;
            let st = match (cur, dir) {
                (true, _) => Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
                (false, true) => Style::default().fg(th.subtext),
                _ => Style::default().fg(th.dim),
            };
            let t: String = format!(" {n}{}", if *dir { "/" } else { "" }).chars().take(w).collect();
            let l = Line::styled(t, st);
            if cur { l.style(Style::default().bg(th.selection)) } else { l }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_preview(f: &mut Frame, app: &mut App, area: Rect, path: Option<&Path>, line: Option<usize>, show: Show) {
    let diff = show == Show::Diff;
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
    let c = app.preview.get(path, line, body.width, &th, hidden, crate::preview::PREVIEW_LINES, show);
    let base = c.focus.saturating_sub(body.height as usize / 3);
    let start = (base + app.preview_scroll).min(c.lines.len().saturating_sub(1));
    let lines: Vec<Line> = c.lines.iter().skip(start).take(body.height as usize).cloned().collect();
    f.render_widget(Paragraph::new(lines), Rect { x: body.x + 1, width: body.width.saturating_sub(1), ..body });
}

// ── viewer ──────────────────────────────────────────────────────────────────

fn draw_viewer(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.th.clone();
    let hidden = app.hidden;
    if let Some(v) = app.viewer.as_mut() {
        if let (true, Some(dv)) = (v.diff, v.dv.as_mut()) {
            let body = crate::diffview::draw(f, &th, dv, &app.viewed, area);
            app.list_area = body;
            return;
        }
    }
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let gutter_marks = viewer_gutter(app);
    let Some(v) = app.viewer.as_mut() else { return };
    let show = if v.log { Show::Log } else { Show::File };
    let c = app.viewer_cache.get(&v.path, v.line, body.width.saturating_sub(1), &th, hidden, crate::preview::VIEWER_LINES, show);
    let name = v.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = v.path.parent().map(tilde).unwrap_or_default();
    let (ic, col) = entry::icon(&name, Kind::File);
    let mut hs = vec![
        Span::styled(format!(" {ic} "), Style::default().fg(icon_color(&th, col))),
        Span::styled(format!("{dir}/"), Style::default().fg(th.subtext)),
        Span::styled(name, Style::default().fg(th.text).add_modifier(Modifier::BOLD)),
    ];
    if !c.ready() {
        // the worker is still rendering: keep the position for when it lands
        f.render_widget(Paragraph::new(Line::from(hs)), head);
        f.render_widget(Paragraph::new(Line::styled("  loading…", Style::default().fg(th.dim).add_modifier(Modifier::ITALIC))), body);
        return;
    }
    let h = body.height as usize;
    v.height = h;
    v.total = c.lines.len();
    let max_top = v.total.saturating_sub(h);
    let top = v.top.unwrap_or_else(|| c.focus.saturating_sub(h / 3)).min(max_top);
    v.top = Some(top);
    let meta = std::fs::metadata(&v.path).ok();
    let mut pos = if v.total == 0 { String::new() } else { format!("{}–{} of {} ", top + 1, (top + h).min(v.total), v.total) };
    if v.log {
        pos = format!("git log   {pos}");
    }
    if !v.search.is_empty() && !v.hits.is_empty() {
        let i = v.at.and_then(|a| v.hits.iter().position(|&x| x == a)).map(|i| i + 1).unwrap_or(0);
        pos = format!("/{} {i}/{}   {pos}", v.search, v.hits.len());
    }
    if let Some(m) = &meta {
        hs.push(Span::styled(format!("  {}  {}", entry::human_size(m.len()), entry::age(m.modified().ok())), Style::default().fg(th.dim)));
    }
    if !gutter_marks.is_empty() {
        hs.push(Span::styled("  ● changed — d diff", Style::default().fg(th.yellow)));
    }
    let used: usize = hs.iter().map(|s| s.content.width()).sum();
    hs.push(Span::raw(" ".repeat((head.width as usize).saturating_sub(used + pos.width()))));
    hs.push(Span::styled(pos, Style::default().fg(th.dim)));
    f.render_widget(Paragraph::new(Line::from(hs)), head);
    // cache index → file line, for the change bars
    let start = v.line.map(|l| l.saturating_sub(crate::preview::VIEWER_LINES / 4).max(1)).unwrap_or(1);
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
            let change = (!v.log).then(|| gutter_marks.get(&(start + i)).copied()).flatten();
            gutter.push(match (hit, change) {
                (true, _) => Line::styled("▌", Style::default().fg(if v.at == Some(i) { th.yellow } else { th.peach })),
                (false, Some('+')) => Line::styled("│", Style::default().fg(th.green)),
                (false, Some('~')) => Line::styled("│", Style::default().fg(th.yellow)),
                (false, Some('_')) => Line::styled("▁", Style::default().fg(th.red)),
                _ => Line::raw(" "),
            });
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

/// Changed lines of the viewed file against HEAD: `+` added, `~` modified,
/// `_` lines removed below this one. Cached per path and mtime.
fn viewer_gutter(app: &mut App) -> HashMap<usize, char> {
    let Some(v) = app.viewer.as_ref() else { return HashMap::new() };
    if !app.cfg.git_status || v.log {
        return HashMap::new();
    }
    let mtime = std::fs::metadata(&v.path).and_then(|m| m.modified()).ok();
    if let Some((p, t, m)) = &app.gutter {
        if *p == v.path && *t == mtime {
            return m.clone();
        }
    }
    let mut marks = HashMap::new();
    if v.path.is_file() {
        if let Ok(files) = crate::diff::load(&v.path, false, crate::diff::Opts { context: 0, ..Default::default() }) {
            for h in files.iter().flat_map(|f| &f.hunks) {
                let dels = h.lines.iter().filter(|l| l.kind == crate::diff::Kind::Del).count();
                let adds: Vec<usize> = h.lines.iter().filter_map(|l| (l.kind == crate::diff::Kind::Add).then_some(l.new_no).flatten()).collect();
                for (k, n) in adds.iter().enumerate() {
                    marks.insert(*n, if k < dels { '~' } else { '+' });
                }
                if adds.is_empty() && dels > 0 {
                    marks.insert(h.new_start.max(1), '_');
                }
            }
        }
    }
    app.gutter = Some((v.path.clone(), mtime, marks.clone()));
    marks
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

/// The finder's source chip colour.
fn kind_color(th: &Theme, k: FinderKind) -> Color {
    match k {
        FinderKind::All | FinderKind::Files => th.accent,
        FinderKind::Dirs => th.blue,
        FinderKind::Grep => th.yellow,
        FinderKind::Recent => th.teal,
        FinderKind::Git => th.green,
        FinderKind::Dash => th.peach,
        FinderKind::Projects => th.red,
    }
}

/// ␣ menu rows: key, source, what it lists.
const SOURCES: &[(char, FinderKind, &str)] = &[
    ('a', FinderKind::All, "files and folders · empty: dash"),
    ('f', FinderKind::Files, "files only"),
    ('d', FinderKind::Dirs, "folders only"),
    ('g', FinderKind::Grep, "file contents (ripgrep)"),
    ('r', FinderKind::Recent, "opened through oil"),
    ('s', FinderKind::Git, "changed & untracked"),
    ('p', FinderKind::Projects, "hop to a project"),
    ('h', FinderKind::Dash, "pending, changes, marks, recent"),
];

fn draw_finder_menu(f: &mut Frame, th: &Theme, area: Rect, cur: FinderKind) {
    let lines: Vec<Line> = SOURCES
        .iter()
        .map(|(key, k, what)| {
            let on = *k == cur;
            let sig = k.sigil().map(|c| format!("{c} ")).unwrap_or_else(|| "  ".into());
            Line::from(vec![
                Span::styled(format!(" {key} "), Style::default().fg(th.yellow).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {:<9}", k.label()), Style::default().fg(kind_color(th, *k)).add_modifier(if on { Modifier::BOLD | Modifier::UNDERLINED } else { Modifier::BOLD })),
                Span::styled(sig, Style::default().fg(th.accent)),
                Span::styled(format!("{what} "), Style::default().fg(th.subtext)),
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
    let blk = frame(th, "␣ source", th.accent);
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    f.render_widget(Paragraph::new(lines), inner);
}

/// `?` in the finder: sources, keys for this source's rows, view keys,
/// query syntax. The only complete list; letters still pick a source.
fn draw_finder_sheet(f: &mut Frame, th: &Theme, area: Rect, cur: FinderKind) {
    let rows: &[(&str, &str)] = match cur {
        FinderKind::Projects => &[("⏎", "hop (focus or open its workspace)"), ("^t", "new tab"), ("^o", "explore"), ("^f alt-g", "files / grep inside"), ("^n", "new project from query"), ("^x", "forget"), ("^k ^k", "close its workspace or tab")],
        FinderKind::Git => &[("⏎", "diff view"), ("^a", "stage ⇄ unstage"), ("alt-d", "discard"), ("^space", "mark"), ("^s", "send")],
        _ => &[("⏎", "open"), ("^v ^x", "split right / down"), ("^t ^e", "tab / popup"), ("^g", "system app"), ("^o", "reveal"), ("^y", "copy path"), ("^space", "mark"), ("^s alt-s", "send paths / lines")],
    };
    let mut view: Vec<(&str, &str)> = vec![("⇥", "next group"), ("^r", "root"), ("alt-h alt-i", "hidden / ignored"), ("alt-p", "preview"), ("esc", "back")];
    if cur == FinderKind::Projects {
        view.insert(0, ("^a", "scope: all → open → dirty → recent"));
    }
    let syntax = match cur {
        FinderKind::Grep => Some("foo -- *.rs -t py   (smart-case)"),
        FinderKind::Projects | FinderKind::Git | FinderKind::Dash => None,
        _ => Some("'exact ^prefix suffix$ !not"),
    };
    let key = Style::default().fg(th.accent).add_modifier(Modifier::BOLD);
    let head = |t: &str| Span::styled(format!(" {t:<8}"), Style::default().fg(th.dim));
    let mut lines = vec![];
    let mut src = vec![head("source")];
    for k in [FinderKind::Grep, FinderKind::Projects, FinderKind::Git, FinderKind::Recent] {
        src.push(Span::styled(format!("{} ", k.sigil().unwrap_or(' ')), Style::default().fg(kind_color(th, k)).add_modifier(Modifier::BOLD)));
        src.push(Span::styled(format!("{}  ", k.label().to_lowercase()), Style::default().fg(th.text)));
    }
    src.push(Span::styled("⌫ ", key));
    src.push(Span::styled("all ", Style::default().fg(th.text)));
    lines.push(Line::from(src));
    let mut letters = vec![head(""), Span::styled("␣ ", key), Span::styled("pick: ", Style::default().fg(th.text))];
    for (c, k, _) in SOURCES {
        letters.push(Span::styled(c.to_string(), Style::default().fg(th.yellow).add_modifier(Modifier::BOLD)));
        letters.push(Span::styled(format!(" {}  ", k.label().to_lowercase()), Style::default().fg(if *k == cur { kind_color(th, *k) } else { th.subtext })));
    }
    lines.push(Line::from(letters));
    let section = |name: &str, pairs: &[(&str, &str)], lines: &mut Vec<Line>| {
        lines.push(Line::raw(""));
        // two pairs per line keeps the sheet short; columns line up
        let kw = |c: usize| pairs.iter().skip(c).step_by(2).map(|(k, _)| k.width()).max().unwrap_or(0);
        let vw = pairs.iter().step_by(2).map(|(_, v)| v.width()).max().unwrap_or(0);
        for (i, ch) in pairs.chunks(2).enumerate() {
            let mut sp = vec![head(if i == 0 { name } else { "" })];
            for (c, (k, v)) in ch.iter().enumerate() {
                sp.push(Span::styled(format!("{k}{} ", " ".repeat(kw(c) - k.width())), key));
                let pad = if c == 0 { vw - v.width() + 3 } else { 1 };
                sp.push(Span::styled(format!("{v}{}", " ".repeat(pad)), Style::default().fg(th.text)));
            }
            lines.push(Line::from(sp));
        }
    };
    section(&cur.label().to_lowercase(), rows, &mut lines);
    section("view", &view, &mut lines);
    if let Some(sx) = syntax {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![head("query"), Span::styled(sx, Style::default().fg(th.subtext))]));
    }
    let w = lines.iter().map(|l| l.width()).max().unwrap_or(20) as u16 + 2;
    let h = lines.len() as u16 + 2;
    let r = Rect {
        x: area.right().saturating_sub(w + 1).max(area.x),
        y: area.bottom().saturating_sub(h).max(area.y),
        width: w.min(area.width),
        height: h.min(area.height),
    };
    f.render_widget(Clear, r);
    let blk = frame(th, "? keys", th.accent);
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_finder(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.th.clone();
    let [prompt, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let spin = SPIN[app.spinner % SPIN.len()];
    let preview_on = app.finder_preview && area.width >= 60;
    let transfer = app.transfer.as_ref().map(|t| (t.copy, t.count));
    let idle = app.last_key.elapsed() >= std::time::Duration::from_secs(2);
    let branch = app.buf().branch.as_ref().map(|b| b.badge());
    let Some(fd) = app.finder.as_mut() else { return };

    // left: the sigil (or MOVE / COPY), then the query or a noun
    let ph = match fd.kind {
        _ if transfer.is_some() => "folder",
        FinderKind::Grep => "file contents",
        FinderKind::Dirs => "folders",
        FinderKind::Recent => "recent files",
        FinderKind::Git => "changed files",
        FinderKind::Dash => "pending, changed, marked, recent",
        FinderKind::Files => "files",
        FinderKind::All => "files & folders",
        FinderKind::Projects => "project or branch",
    };
    let mut ps = vec![Span::raw(" ")];
    if let Some((copy, n)) = transfer {
        let label = format!(" {} {n} → ", if copy { "COPY" } else { "MOVE" });
        ps.push(Span::styled(label, Style::default().fg(Color::Black).bg(if copy { th.blue } else { th.peach }).add_modifier(Modifier::BOLD)));
    } else {
        let sig = fd.kind.sigil().unwrap_or('›');
        ps.push(Span::styled(sig.to_string(), Style::default().fg(kind_color(&th, fd.kind)).add_modifier(Modifier::BOLD)));
        if fd.query.is_empty() && fd.kind.sigil().is_some() {
            // a bare sigil is easy to miss: name the source while nothing is typed
            ps.push(Span::styled(format!(" {}", fd.kind.label().to_lowercase()), Style::default().fg(th.dim)));
        }
    }
    ps.push(Span::raw(" "));
    let lead_w: usize = ps.iter().map(|s| s.content.width()).sum();
    if fd.query.is_empty() {
        ps.push(Span::styled(ph, Style::default().fg(th.dim).add_modifier(Modifier::ITALIC)));
    } else {
        ps.push(Span::styled(fd.query.clone(), Style::default().fg(th.text)));
    }
    if !fd.marked.is_empty() {
        ps.push(Span::styled(format!("   ● {} marked", fd.marked.len()), Style::default().fg(th.peach)));
    }

    // hint on hesitation: an idle, empty All / Files prompt names the sigils
    if idle && fd.query.is_empty() && transfer.is_none() && matches!(fd.kind, FinderKind::All | FinderKind::Files) {
        let mut hint = vec![Span::styled("   or start with ", Style::default().fg(th.dim))];
        for k in [FinderKind::Grep, FinderKind::Projects, FinderKind::Git, FinderKind::Recent] {
            hint.push(Span::styled(k.sigil().unwrap_or(' ').to_string(), Style::default().fg(kind_color(&th, k)).add_modifier(Modifier::BOLD)));
            hint.push(Span::styled(format!(" {}  ", k.label().to_lowercase()), Style::default().fg(th.dim)));
        }
        let hw: usize = hint.iter().map(|s| s.content.width()).sum();
        let used: usize = ps.iter().map(|s| s.content.width()).sum();
        if used + hw <= prompt.width as usize {
            ps.extend(hint);
        }
    }

    // right: count · root, flags only when not default
    let filtering = !fd.query.is_empty();
    let count = match fd.kind {
        FinderKind::Grep => format!("{} hits", fd.len()),
        _ if fd.zero_view() => format!("{} to triage", fd.len()),
        _ if filtering => format!("{} of {}", fd.len(), fd.total()),
        _ => fd.total().to_string(),
    };
    let count = if fd.walking() { format!("{spin} {count}") } else { count };
    let mut flags = String::new();
    if fd.hidden {
        flags.push_str(" +hidden");
    }
    if fd.no_ignore {
        flags.push_str(" +ignored");
    }
    let trunc = if fd.truncated() { " (capped)" } else { "" };
    let root = match fd.kind {
        FinderKind::Git => branch.unwrap_or_else(|| tilde(&fd.root)),
        FinderKind::Projects => {
            let roots = &fd.proj_settings.roots;
            let first = roots.first().map(|r| tilde(r)).unwrap_or_default();
            let more = if roots.len() > 1 { format!(" +{}", roots.len() - 1) } else { String::new() };
            let scope = if fd.proj_scope == crate::projects::Scope::All { String::new() } else { format!("{} · ", fd.proj_scope.label()) };
            format!("{scope}{first}{more}")
        }
        _ => tilde(&fd.root),
    };
    let used: usize = ps.iter().map(|s| s.content.width()).sum();
    let width = prompt.width as usize;
    // the path goes first on a narrow popup (shortened, then dropped), then the count
    let fixed = count.width() + flags.width() + trunc.len() + 2;
    let budget = width.saturating_sub(used + fixed + 5).min(width * 2 / 5);
    let mut right = if budget >= 8 {
        let r = if root.width() > budget {
            let tail: String = root.chars().rev().take(budget - 1).collect::<Vec<_>>().into_iter().rev().collect();
            format!("…{tail}")
        } else {
            root
        };
        format!("{count} · {r}{flags}  ")
    } else {
        format!("{count}{flags}  ")
    };
    if used + right.width() + trunc.len() + 2 > width {
        right.clear();
    }
    let used: usize = ps.iter().map(|s| s.content.width()).sum();
    let pad = width.saturating_sub(used + right.width() + trunc.len());
    ps.push(Span::raw(" ".repeat(pad)));
    ps.push(Span::styled(right, Style::default().fg(th.dim)));
    ps.push(Span::styled(trunc, Style::default().fg(th.peach)));
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
            sp.push(Span::styled(format!("{} ", g.letter), Style::default().fg(git_color(&th, g)).add_modifier(Modifier::BOLD)));
        } else if let Some(t) = it.tag {
            let col = match t {
                '+' => th.green,
                '~' => th.yellow,
                '→' | '⧉' => th.blue,
                '−' => th.red,
                '●' => th.peach,
                _ => th.dim,
            };
            sp.push(Span::styled(format!("{t} "), Style::default().fg(col).add_modifier(Modifier::BOLD)));
        }
        if let Some(p) = &it.proj {
            sp.pop();
            sp.extend(project_row(&th, p, &it.display, &it.indices, w, hl));
            let mut line = Line::from(sp);
            if is_sel {
                line = line.style(Style::default().bg(th.selection));
            }
            lines.push(line);
            continue;
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
            FinderKind::Dash if query.is_empty() => "nothing pending, changed, marked or recent",
            FinderKind::All if query.is_empty() => "nothing pending, changed, marked or recent — type to find files",
            FinderKind::Projects if app.finder.as_ref().is_some_and(|f| f.walking()) => "looking for projects…",
            FinderKind::Projects if query.is_empty() => "no projects here — set roots in [projects] (or herdr-hop's config) · ^a scope",
            FinderKind::Projects => "no matching project — ^n creates it",
            _ => "no matches",
        };
        lines.push(Line::styled(format!("  {msg}"), Style::default().fg(th.dim).add_modifier(Modifier::ITALIC)));
    }
    app.list_area = list_inner;
    f.render_widget(Paragraph::new(lines), list_inner);
    let sel_item = items.into_iter().nth(sel.saturating_sub(scroll)).filter(|_| sel >= scroll);
    let q = app.finder.as_ref().map(|f| f.query.clone()).unwrap_or_default();
    app.finder_foot = finder_foot(kind, sel_item.as_ref(), marked.len(), transfer, &q);
    if let Some(p) = prev {
        let inner = p;
        match sel_item {
            Some(it) => {
                let show = match (&it.proj, it.git) {
                    (Some(_), _) => Show::Project,
                    (_, Some(_)) => Show::Diff,
                    _ => Show::File,
                };
                draw_preview(f, app, inner, Some(&it.abs), it.line, show)
            }
            None => draw_preview(f, app, inner, None, None, Show::File),
        }
    }
    if app.finder_sheet {
        draw_finder_sheet(f, &th, body, kind);
    } else if app.finder_menu {
        let cur = app.finder.as_ref().map(|f| f.kind).unwrap_or(FinderKind::All);
        draw_finder_menu(f, &th, body, cur);
    }
}

/// Finder footer: at most two verbs for the selected row (or the marks).
fn finder_foot(kind: FinderKind, it: Option<&crate::finder::Item>, marked: usize, transfer: Option<(bool, usize)>, query: &str) -> Vec<(String, String)> {
    let p = |k: &str, v: String| (k.to_string(), v);
    if let Some((copy, n)) = transfer {
        let verb = if copy { "copy" } else { "move" };
        return vec![p("⏎", format!("{verb} {n} here")), p("^n", "new folder".into())];
    }
    if marked > 0 {
        return match kind {
            FinderKind::Git => vec![p("^a", format!("stage {marked}")), p("^s", format!("send {marked}"))],
            _ => vec![p("⏎", format!("open {marked}")), p("^s", format!("send {marked}"))],
        };
    }
    let Some(it) = it else {
        return match kind {
            FinderKind::Projects if !query.trim().is_empty() => vec![p("^n", format!("create \"{}\"", query.trim()))],
            _ => vec![],
        };
    };
    if let Some(pr) = &it.proj {
        return match pr.ws {
            Some(_) => vec![p("⏎", "focus workspace".into()), p("^t", "tab".into())],
            None => vec![p("⏎", "open workspace".into()), p("^o", "explore".into())],
        };
    }
    if kind == FinderKind::Git {
        let staged = it.git.is_some_and(|g| g.staged && !g.unstaged);
        return vec![p("⏎", "diff".into()), p("^a", if staged { "unstage" } else { "stage" }.into())];
    }
    if it.is_dir {
        return vec![p("⏎", "explore".into())];
    }
    match it.line {
        Some(l) => vec![p("⏎", format!("open at {l}")), p("^v", "split".into())],
        None => vec![p("⏎", "open".into()), p("^v", "split".into())],
    }
}

/// A Projects row: recent `▎` and open-workspace `▶` markers, stack icon,
/// name (parent dimmed) with `◆` on the project oil was opened in, time
/// since the last hop, branch, `●N` changes / `↑N` unpushed / `○` clean,
/// agents and their state. `key` is the matched string (name, two spaces, branch).
fn project_row(th: &crate::theme::Theme, p: &crate::projects::Proj, key: &str, idx: &[u32], w: usize, hl: Style) -> Vec<Span<'static>> {
    let mut sp = vec![
        Span::styled(if p.hopped.is_some() { "▎" } else { " " }, Style::default().fg(th.accent)),
        Span::styled(if p.ws.is_some() { "\u{25b6} " } else { "  " }, Style::default().fg(th.accent)),
        Span::styled(format!("{} ", p.icon.0), Style::default().fg(icon_color(th, p.icon.1))),
    ];
    let aligned = key.starts_with(p.name.as_str());
    let name_chars = p.name.chars().count() as u32;
    let split = p.name.rfind('/').map(|i| i + 1).unwrap_or(0);
    let dir_chars = p.name[..split].chars().count() as u32;
    let (dpart, fpart) = p.name.split_at(split);
    let pick = |lo: u32, hi: u32| -> Vec<u32> { if aligned { idx.iter().copied().filter(|&i| i >= lo && i < hi).map(|i| i - lo).collect() } else { vec![] } };
    sp.extend(highlight(dpart, &pick(0, dir_chars), Style::default().fg(th.subtext), hl));
    sp.extend(highlight(fpart, &pick(dir_chars, name_chars), Style::default().fg(th.text).add_modifier(Modifier::BOLD), hl));
    let col = if w < 80 { (w / 4).max(14) } else { (w / 3).min(32) };
    let mut used = p.name.width();
    if p.here {
        sp.push(Span::styled(" ◆", Style::default().fg(th.teal)));
        used += 2;
    }
    sp.push(Span::raw(" ".repeat(col.saturating_sub(used) + 1)));
    let age = p.hopped.map(|t| {
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(t);
        let fresh = at.elapsed().is_ok_and(|d| d.as_secs() < 600);
        (entry::age(Some(at)), fresh)
    });
    match age {
        Some((a, fresh)) => sp.push(Span::styled(format!("{a:<4} "), Style::default().fg(if fresh { th.accent } else { th.dim }))),
        None => sp.push(Span::raw("     ")),
    }
    match &p.head {
        Some(h) => {
            let c = if h.detached { th.peach } else { th.green };
            let shown: String = h.name.chars().take(14).collect();
            sp.extend(highlight(&shown, &pick(name_chars + 2, u32::MAX), Style::default().fg(c), hl));
            sp.push(Span::raw(" ".repeat(14usize.saturating_sub(shown.width()) + 1)));
            if let Some(r) = &h.worktree_of {
                sp.push(Span::styled(format!("({r}) "), Style::default().fg(th.dim)));
            }
        }
        None => sp.push(Span::styled(format!("{:<15}", "—"), Style::default().fg(th.dim))),
    }
    match p.dirty {
        Some((0, 0)) => sp.push(Span::styled("○", Style::default().fg(th.dim))),
        Some((n, a)) => {
            if n > 0 {
                sp.push(Span::styled(format!("●{n}"), Style::default().fg(th.red)));
            }
            if a > 0 {
                sp.push(Span::styled(format!(" ↑{a}"), Style::default().fg(th.yellow)));
            }
        }
        None => {}
    }
    if let Some(ws) = p.ws.as_ref().filter(|ws| ws.agents > 0) {
        let c = match ws.status.as_str() {
            "working" => th.green,
            "waiting" => th.yellow,
            _ => th.dim,
        };
        sp.push(Span::styled(format!(" {}\u{25b6} {}", ws.agents, ws.status), Style::default().fg(c)));
    }
    sp
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
        ("D", "diff of this entry (a folder: all its changes)"),
        ("l", "git log of this entry"),
        ("G", "only changed entries ⇄ all"),
        ("w", "parent-folder column"),
        ("?", "all keys"),
    ]),
    ("]", &[("g", "next changed entry"), ("c", "next changed entry")]),
    ("[", &[("g", "previous changed entry"), ("c", "previous changed entry")]),
    (" ", &[("a", "find anything"), ("f", "find files"), ("g", "grep"), ("d", "find folders"), ("r", "recent files"), ("s", "git changes"), ("p", "projects (hop)"), ("h", "dashboard"), ("␣", "find anything")]),
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
        (View::Viewer, _) => app.viewer.as_ref().and_then(|v| v.typing.clone()).map(|(t, c)| ('/', t, c)),
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
    if app.view == View::Finder {
        draw_finder_status(f, app, area);
        return;
    }
    let (label, bg) = match (app.view, &app.vim.mode) {
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
        (View::Viewer, _) if app.viewer.as_ref().is_some_and(|v| v.diff) => &[("]c [c", "hunk"), ("⇥", "file"), ("s", "split"), ("w", "whitespace"), ("z", "expand"), ("v", "viewed"), ("c", "compare"), ("⏎", "open at line"), ("q", "back")],
        (View::Viewer, _) => &[("j/k ␣ b", "scroll"), ("/ n N", "search"), ("d", "diff"), ("L", "log"), ("e", "edit here"), ("p", "send"), ("q", "back")],
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

/// Finder footer: messages on the left, the selected row's verbs and a
/// fixed `? keys` on the right. No mode badge: the prompt's sigil is the mode.
fn draw_finder_status(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.th;
    let mut sp = vec![Span::raw(" ")];
    if let Some((m, k, _)) = &app.msg {
        let c = match k {
            MsgKind::Info => th.subtext,
            MsgKind::Ok => th.green,
            MsgKind::Warn => th.yellow,
            MsgKind::Err => th.red,
        };
        sp.push(Span::styled(m.clone(), Style::default().fg(c)));
    }
    let mut hs = Vec::new();
    for (i, (k, v)) in app.finder_foot.iter().enumerate() {
        if i > 0 {
            hs.push(Span::styled("  ·", Style::default().fg(th.dim)));
        }
        hs.push(Span::styled(format!(" {k}"), Style::default().fg(th.accent)));
        hs.push(Span::styled(format!(" {v}"), Style::default().fg(th.subtext)));
    }
    let keys = [Span::styled("    ? ", Style::default().fg(th.accent).add_modifier(Modifier::BOLD)), Span::styled("keys ", Style::default().fg(th.dim))];
    let used: usize = sp.iter().map(|s| s.content.width()).sum();
    let kw: usize = keys.iter().map(|s| s.content.width()).sum();
    let mut hw: usize = hs.iter().map(|s| s.content.width()).sum();
    if used + hw + kw > area.width as usize {
        hs.clear();
        hw = 0;
    }
    if used + hw + kw <= area.width as usize {
        sp.push(Span::raw(" ".repeat(area.width as usize - used - hw - kw)));
        sp.extend(hs);
        sp.extend(keys);
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
    let Overlay::Confirm { plan, undo, sel, .. } = &app.overlay else { return };
    let sel = *sel;
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
        let cur = lines.len() == sel;
        let mut sp = vec![
            Span::styled(if cur { "▌" } else { " " }, Style::default().fg(th.accent)),
            Span::styled(format!("{tag:<7}"), Style::default().fg(col).add_modifier(Modifier::BOLD)),
        ];
        sp.extend(body);
        let mut l = Line::from(sp);
        if cur {
            l = l.style(Style::default().bg(th.selection));
        }
        lines.push(l);
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
    // problems left after dropping ops (x): applying waits until they are gone
    if !plan.errors.is_empty() {
        lines.push(Line::raw(""));
        for e in &plan.errors {
            lines.push(Line::styled(format!(" ⚠ {e}"), Style::default().fg(th.red)));
        }
    }
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
    // keep the cursor row (2 summary rows above the ops) in view
    let bh = body.height as usize;
    let max_scroll = lines.len().saturating_sub(bh);
    let scroll = (sel + 3).saturating_sub(bh).min(max_scroll);
    let shown: Vec<Line> = lines.into_iter().skip(scroll).collect();
    f.render_widget(Paragraph::new(shown), body);
    let k = |t: &'static str, c: Color| Span::styled(t, Style::default().fg(c).add_modifier(Modifier::BOLD));
    let d = |t: &'static str| Span::styled(t, Style::default().fg(th.dim));
    f.render_widget(
        Paragraph::new(Line::from(vec![
            k(" y/⏎", th.green),
            d(" apply   "),
            k("n/esc", th.red),
            d(" keep editing   "),
            k("j/k", th.accent),
            d(" pick   "),
            k("x", th.accent),
            d(" drop   "),
            k("o", th.accent),
            d(" go there"),
        ])),
        foot,
    );
}

fn draw_ask(f: &mut Frame, th: &Theme, area: Rect, title: &str, items: &[String]) {
    let mut lines: Vec<Line> = items.iter().take(12).map(|l| Line::styled(format!("   {l}"), Style::default().fg(th.text))).collect();
    if items.len() > 12 {
        lines.push(Line::styled(format!("   … {} more", items.len() - 12), Style::default().fg(th.dim)));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(" y", Style::default().fg(th.red).add_modifier(Modifier::BOLD)),
        Span::styled(" yes   ", Style::default().fg(th.dim)),
        Span::styled("n/esc", Style::default().fg(th.accent).add_modifier(Modifier::BOLD)),
        Span::styled(" cancel", Style::default().fg(th.dim)),
    ]));
    let w = lines.iter().map(|l| l.width()).max().unwrap_or(40).max(title.width() + 6) as u16 + 4;
    let r = centered(area, w, lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    let blk = frame(th, title, th.red);
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_history(f: &mut Frame, app: &App, area: Rect, sel: usize) {
    let th = &app.th;
    let mut lines = Vec::new();
    let mut cur_row = 0;
    for (k, b) in app.journal.iter().rev().enumerate() {
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(b.when);
        let state = match (b.undo.is_empty(), b.permanent) {
            (true, _) => ("permanent", th.red),
            (false, 0) => ("undoable", th.green),
            (false, _) => ("partly undoable", th.yellow),
        };
        let cur = k == sel;
        if cur {
            cur_row = lines.len();
        }
        let mut l = Line::from(vec![
            Span::styled(if cur { "▌" } else { " " }, Style::default().fg(th.accent)),
            Span::styled(format!("{:>4} ago  ", entry::age(Some(when))), Style::default().fg(th.dim)),
            Span::styled(format!("{} change{}", b.lines.len(), if b.lines.len() == 1 { "" } else { "s" }), Style::default().fg(th.text).add_modifier(Modifier::BOLD)),
            Span::styled(format!("   {}", state.0), Style::default().fg(state.1)),
        ]);
        if cur {
            l = l.style(Style::default().bg(th.selection));
        }
        lines.push(l);
        let show = if cur { 12 } else { 2 };
        for op in b.lines.iter().take(show) {
            lines.push(Line::styled(format!("        {op}"), Style::default().fg(if cur { th.subtext } else { th.dim })));
        }
        if b.lines.len() > show {
            lines.push(Line::styled(format!("        … {} more", b.lines.len() - show), Style::default().fg(th.dim)));
        }
    }
    let r = centered(area, 96, area.height.saturating_sub(2));
    f.render_widget(Clear, r);
    let blk = frame(th, "Applied batches (newest first)", th.accent);
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    let [body, foot] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let skip = cur_row.saturating_sub(body.height as usize / 3);
    f.render_widget(Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()), body);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" j/k", Style::default().fg(th.accent).add_modifier(Modifier::BOLD)),
            Span::styled(" pick   ", Style::default().fg(th.dim)),
            Span::styled("u/⏎", Style::default().fg(th.green).add_modifier(Modifier::BOLD)),
            Span::styled(" review undoing it   ", Style::default().fg(th.dim)),
            Span::styled("y", Style::default().fg(th.accent).add_modifier(Modifier::BOLD)),
            Span::styled(" copy   ", Style::default().fg(th.dim)),
            Span::styled("esc", Style::default().fg(th.red).add_modifier(Modifier::BOLD)),
            Span::styled(" close", Style::default().fg(th.dim)),
        ])),
        foot,
    );
}

fn draw_conflicts(f: &mut Frame, th: &Theme, area: Rect, items: &[String], scroll: usize) {
    let ls: Vec<Line> = items.iter().map(|l| Line::styled(format!(" {l}"), Style::default().fg(th.text))).collect();
    let w = ls.iter().map(|l| l.width()).max().unwrap_or(30).max(64) as u16 + 4;
    let r = centered(area, w, ls.len() as u16 + 4);
    f.render_widget(Clear, r);
    let blk = frame(th, "Cannot save — names clash", th.red);
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    let [body, foot] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    f.render_widget(Paragraph::new(ls.into_iter().skip(scroll).collect::<Vec<_>>()), body);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" c", Style::default().fg(th.green).add_modifier(Modifier::BOLD)),
            Span::styled(" number the clashing names   ", Style::default().fg(th.dim)),
            Span::styled("o", Style::default().fg(th.accent).add_modifier(Modifier::BOLD)),
            Span::styled(" go to the first   ", Style::default().fg(th.dim)),
            Span::styled("any key", Style::default().fg(th.accent).add_modifier(Modifier::BOLD)),
            Span::styled(" back to editing", Style::default().fg(th.dim)),
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
        ("-  ⌫", "parent directory (or click a path segment in the header)"),
        ("_  ~", "start directory · home"),
        ("^v ^x ^t", "open in split right / below / new tab"),
        ("^e", "edit in $EDITOR inside the popup, come back after"),
        ("H  L", "back · forward through visited folders"),
        ("m{a-z}  '{a-z}  :marks", "mark this folder · jump to a mark · list"),
        ("gx", "open with system app"),
        ("gy  gY  gn", "copy relative path · absolute path · name"),
        ("gi  :'<,'>send", "type path(s) into the origin pane (@path for agents)"),
        ("ge", "edit this listing in $EDITOR (/001 tags keep identity)"),
        ("g.  gs  gS  gd", "hidden · cycle sort · reverse · sizes → details → meta → off"),
        ("gw", "parent-folder column (wide popups)"),
        ("^p", "toggle preview (^f/^b scroll it)"),
        ("^l  :e!", "refresh · discard edits here"),
        ("g?  then /", "this help · type to filter it"),
        ("q  esc  ZZ  ZQ", "quit (asks if unsaved) · apply+quit · discard+quit"),
    ]),
    ("Git", &[
        ("]g [g  ]c [c", "next · previous changed entry"),
        ("gD  :diff", "diff view of the entry (a folder: all its changes)"),
        ("gl  :log", "git log of the entry"),
        ("gG  :changed", "only entries with a git status ⇄ all"),
        (":stage  :unstage", "on the line or a range (:'<,'>stage)"),
        (":restore", "discard unstaged changes (asks first)"),
        ("status colours", "green staged · yellow unstaged · peach both · teal new"),
    ]),
    ("Viewer (⏎ on a file)", &[
        ("j k  ␣ b  ^d ^u  g G", "scroll · page · half page · top/bottom"),
        ("/pattern  n N", "search the file · next / previous hit"),
        ("d", "diff view (on for the Git tab and deleted files)"),
        ("L", "git log of the file"),
        ("p  P", "send path (:line of the hit) · send the lines around it"),
        ("e", "edit in $EDITOR, back to the viewer after"),
        ("^v ^x ^t  o", "open in split / below / tab · system app"),
        ("y", "copy path"),
        ("gutter │ ▁", "lines changed against HEAD (green new, yellow edited)"),
        ("q  esc  h  -", "back to the listing (or finder)"),
    ]),
    ("Diff view", &[
        ("j k  ␣ b  g G", "move the cursor"),
        ("]c [c", "next · previous hunk"),
        ("⇥ ⇧⇥", "next · previous file"),
        ("s", "unified ⇄ split"),
        ("w", "ignore whitespace"),
        ("z  Z", "expand 20 more lines of context · the whole file"),
        ("v", "mark viewed (and go to the next file)"),
        ("c", "compare: working tree · staged · unstaged"),
        ("⏎  e  ^v ^x ^t", "open $EDITOR at the cursor's line"),
        ("p  P", "send path:line · send the hunk to the agent"),
        ("h l", "scroll sideways"),
        ("/ n N  ^l", "search · reload"),
        ("d", "back to the plain file"),
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
        (":%s/^/{03#}_/", "counter: {#} 1 2 3 · {03#} 001 · {#10} from 10 · {#0+5} step"),
        (":g/pat/d  :v/pat/d", "delete lines matching / not matching (previewed)"),
        (":g/pat/s/a/b/", "substitute on matching lines only"),
        ("V.. :s  I  A", "same on a selection · prefix · suffix the names"),
        (":prefix x  :suffix x", "prepend · append before the extension ({#} works)"),
        (":recipe kebab", "kebab snake slugify lower upper title clean screenshots"),
        ("name -> target", "symlink: edit the target to relink, new line creates"),
        ("U  :undo-apply", "undo the last apply (U again redoes)"),
        (":history", "applied batches · review undoing an older one"),
        (":plan-copy  :plan > f", "copy · write the pending plan as text"),
        (":chmod +x  :%chmod 644", "change permissions (applies at once)"),
        (":extract  :zip a.zip", "unpack into a new folder · pack the range (:tar x.tgz)"),
        (":w  ^s", "review & apply (j/k pick, x drop, o go there)"),
        (":touch a b  :mkdir d", "add entries quickly"),
        (":cd path ⇥", "jump, ⇥ completes paths, commands and arguments"),
        ("↑ ↓  ^p ^n", "in : and / — history (matching what is typed)"),
        ("^a ^e ^w ^u ^k  alt-b alt-f", "line editing in : / and the finder"),
        ("/pat  :noh", "search lines (matches highlighted) · clear"),
        (":sort mtime  :sort!", "sort name|mtime|size|ext · reverse"),
    ]),
    ("Vim motions", &[
        ("h j k l w b e 0 ^ $", "motions, with counts"),
        ("gg G {n}G", "first / last / nth line"),
        ("f F t T ; ,", "find char · repeat · reverse"),
        ("i a I A s S C D x r ~ J", "the usual"),
        ("/pattern n N", "search lines"),
    ]),
    ("Finder (⇥ or ␣␣ all · ␣f files · ␣g grep · ␣d dirs · ␣r recent · ␣s git · ␣p projects · ␣h dash)", &[
        ("type", "fuzzy query ('exact ^prefix suffix$ !not)"),
        ("/ @ + <  typed first", "switch to grep · projects · changes · recent (from All or Files)"),
        ("empty query in All", "pending edits, git changes, marked and recent files"),
        ("␣ or ? on empty", "pick a source: a f d g r s p h"),
        ("⌫ on empty", "back to All, then to the explorer"),
        ("foo -- *.rs  -t py", "grep: only these globs / file types"),
        ("⇥ ⇧⇥", "next · previous group (grep: file · dash: section · else folder)"),
        ("^s  alt-s", "send selection / marked paths · their grep lines as text"),
        ("⏎ ^v ^x ^t ^e", "open (default · split · below · tab · popup)"),
        ("Git: ⏎ ^a alt-d", "diff view of all changes · stage ⇄ unstage · discard"),
        ("Dash", "pending edits, git changes, marked and recent files"),
        ("^o", "reveal in explorer"),
        ("^space", "mark files / grep hits (line kept), ⏎ opens all"),
        ("^r", "root: cwd ⇄ repository ⇄ parent"),
        ("alt-h alt-i", "toggle hidden · ignored files"),
        ("^y", "copy path"),
        ("esc", "back to explorer"),
    ]),
    ("Projects (␣p, :projects, the Projects tab; herdr-hop's roots and recents)", &[
        ("type", "fuzzy over name and branch"),
        ("⏎", "hop: focus the workspace open there, else create one"),
        ("^t", "open as a tab in the current workspace"),
        ("^o  ^f  alt-g", "explore it in oil · find files in it · grep in it"),
        ("^n", "new project from the query (mkdir, git init, README, commit), then hop"),
        ("^a", "scope: all → open → dirty → recent"),
        ("^x  ^k ^k", "forget from recent · close its workspace (or just its tab when it shares one)"),
        ("^y ^g ^s", "copy path · system opener · send path"),
        ("▎ ▶ ●N ↑N ○ ◆", "recent · open · changes · unpushed · clean · you are here"),
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
