//! Vim-style modal editing over a [`Buffer`]: counts, operators (d/c/y) with
//! motions and text objects, linewise registers that carry entry ids, visual
//! line mode, `/` search, `:` commands, undo/redo. Explorer commands that
//! are not text edits come back as an [`Action`].

use crate::buffer::{Buffer, Line};
use crate::config::OpenTarget;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinderKind {
    Files,
    Dirs,
    Grep,
    Recent,
    /// Changed / untracked files (`git status`), previewed as diffs.
    Git,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Visual { anchor: usize },
    Cmd { kind: char, text: String, cur: usize, origin: (usize, usize) },
}

#[derive(Clone, Debug)]
pub enum Reg {
    Lines(Vec<Line>),
    Chars(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Open(Option<OpenTarget>),
    Parent,
    Home,
    StartDir,
    TogglePreview,
    ToggleHidden,
    CycleSort,
    ToggleDetails,
    Help,
    Save,
    Quit,
    ForceQuit,
    SaveQuit,
    Refresh,
    Discard,
    Finder(FinderKind),
    YankPath(PathKind),
    Command(String),
    /// Back / forward through visited directories (`H` / `L`).
    HistBack,
    HistFwd,
    SetMark(char),
    JumpMark(char),
    ReverseSort,
    /// `:chmod` on the entries behind these line ids (applied right away).
    Chmod { spec: String, ids: Vec<u64> },
    /// Roll back (or redo) the last applied batch.
    UndoApply,
    /// Edit this listing in `$EDITOR`.
    EditBuffer,
    /// Type the entries' paths into the origin pane.
    Send(Vec<u64>),
    PreviewScroll(i32),
    /// Move (`copy: false`) or copy the yanked/cut lines to a folder picked
    /// with the fuzzy finder.
    Transfer { copy: bool },
}

/// What `gy` / `gY` / `gn` copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    Relative,
    Absolute,
    Name,
}

pub struct Vim {
    pub mode: Mode,
    pending: Vec<char>,
    pub reg: Option<Reg>,
    pub search: String,
    search_char: Option<(char, char)>,
    /// The line register came from a cut (dd) rather than a yank (yy).
    pub reg_cut: bool,
    pub msg: Option<String>,
    /// Keys of the last buffer change, replayed by `.`.
    last_change: Vec<KeyEvent>,
    /// Keys of a change still being typed in insert mode.
    recording: Option<Vec<KeyEvent>>,
    /// Keys of the normal-mode command in progress.
    cmd_keys: Vec<KeyEvent>,
    /// Last visual-line selection, for `:'<,'>`.
    last_visual: Option<(usize, usize)>,
    /// Lines before a live `:s` preview was drawn over them.
    sub_base: Option<Vec<Line>>,
}

fn cls(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// Char-motion helpers on a single line (`v` = chars of the line).
fn next_word_start(v: &[char], col: usize, big: bool) -> usize {
    let n = v.len();
    if col >= n {
        return n;
    }
    let mut i = col;
    let c0 = cls(v[i], big);
    while i < n && cls(v[i], big) == c0 && c0 != 0 {
        i += 1;
    }
    while i < n && cls(v[i], big) == 0 {
        i += 1;
    }
    i
}

fn word_end(v: &[char], col: usize, big: bool) -> usize {
    let n = v.len();
    if n == 0 {
        return 0;
    }
    let mut i = (col + 1).min(n - 1);
    while i < n - 1 && cls(v[i], big) == 0 {
        i += 1;
    }
    let c0 = cls(v[i], big);
    while i + 1 < n && cls(v[i + 1], big) == c0 {
        i += 1;
    }
    i
}

fn word_back(v: &[char], col: usize, big: bool) -> usize {
    if col == 0 || v.is_empty() {
        return 0;
    }
    let mut i = col.min(v.len()) - 1;
    while i > 0 && cls(v[i], big) == 0 {
        i -= 1;
    }
    let c0 = cls(v[i], big);
    while i > 0 && cls(v[i - 1], big) == c0 {
        i -= 1;
    }
    i
}

/// `iw` / `aw` range [a, b).
fn word_object(v: &[char], col: usize, around: bool, big: bool) -> (usize, usize) {
    if v.is_empty() {
        return (0, 0);
    }
    let col = col.min(v.len() - 1);
    let c0 = cls(v[col], big);
    let (mut a, mut b) = (col, col + 1);
    while a > 0 && cls(v[a - 1], big) == c0 {
        a -= 1;
    }
    while b < v.len() && cls(v[b], big) == c0 {
        b += 1;
    }
    if around {
        let mut e = b;
        while e < v.len() && cls(v[e], big) == 0 {
            e += 1;
        }
        if e > b {
            b = e;
        } else {
            while a > 0 && cls(v[a - 1], big) == 0 {
                a -= 1;
            }
        }
    }
    (a, b)
}

/// The "name" object for filenames: `in` = stem (before the last dot),
/// `an` = the extension including the dot.
fn name_object(v: &[char], around: bool) -> (usize, usize) {
    let dot = v.iter().rposition(|&c| c == '.').filter(|&i| i > 0);
    let end = if v.last() == Some(&'/') { v.len() - 1 } else { v.len() };
    match (dot, around) {
        (Some(d), false) => (0, d),
        (Some(d), true) => (d, end),
        (None, false) => (0, end),
        (None, true) => (end, end),
    }
}

fn find_char(v: &[char], col: usize, kind: char, ch: char, count: usize) -> Option<usize> {
    let mut pos = col;
    for _ in 0..count.max(1) {
        pos = match kind {
            'f' => (pos + 1..v.len()).find(|&i| v[i] == ch)?,
            't' => {
                let i = (pos + 2..v.len()).find(|&i| v[i] == ch).or_else(|| (pos + 1..v.len()).find(|&i| v[i] == ch))?;
                i - 1
            }
            'F' => (0..pos).rev().find(|&i| v[i] == ch)?,
            'T' => (0..pos.saturating_sub(1)).rev().find(|&i| v[i] == ch)? + 1,
            _ => return None,
        };
    }
    Some(pos)
}

enum Motion {
    /// Charwise range on the current line, [a, b) after normalisation; the
    /// cursor goes to `to`.
    Char { to: usize, inclusive: bool },
    Range { a: usize, b: usize },
    Lines { to: usize },
    LinesAbs { a: usize, b: usize },
}

enum Parse {
    More,
    Bad,
    Done,
}

impl Default for Vim {
    fn default() -> Self {
        Vim {
            mode: Mode::Normal,
            pending: vec![],
            reg: None,
            search: String::new(),
            search_char: None,
            reg_cut: false,
            msg: None,
            last_change: vec![],
            recording: None,
            cmd_keys: vec![],
            last_visual: None,
            sub_base: None,
        }
    }
}

impl Vim {
    fn set_lines(&mut self, lines: Vec<Line>, cut: bool) {
        self.reg = Some(Reg::Lines(lines));
        self.reg_cut = cut;
    }

    /// Yanked/cut lines that name entries (what gm/gc would carry).
    pub fn reg_entries(&self) -> usize {
        match &self.reg {
            Some(Reg::Lines(ls)) => ls.iter().filter(|l| l.significant()).count(),
            _ => 0,
        }
    }

    pub fn pending_str(&self) -> String {
        self.pending.iter().collect()
    }

    pub fn handle(&mut self, b: &mut Buffer, key: KeyEvent) -> Action {
        match self.mode.clone() {
            Mode::Insert => self.insert_key(b, key),
            Mode::Cmd { .. } => self.cmd_key(b, key),
            Mode::Visual { anchor } => self.visual_key(b, key, anchor),
            Mode::Normal => self.normal_key(b, key),
        }
    }

    // ── insert ──────────────────────────────────────────────────────────────

    /// Leaving insert mode ends the change `.` will repeat.
    fn end_insert(&mut self, b: &mut Buffer) {
        let noop = b.squash_noop();
        if let Some(mut r) = self.recording.take() {
            if !noop {
                r.retain(|k| !(k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('s')));
                r.push(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                self.last_change = r;
            }
        }
    }

    fn insert_key(&mut self, b: &mut Buffer, k: KeyEvent) -> Action {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(r) = self.recording.as_mut() {
            if !matches!(k.code, KeyCode::Esc) {
                r.push(k);
            }
        }
        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                b.col = b.col.saturating_sub(1);
                self.end_insert(b);
                b.clamp();
            }
            KeyCode::Char('c') if ctrl => {
                self.mode = Mode::Normal;
                self.end_insert(b);
                b.clamp();
            }
            KeyCode::Char('w') if ctrl => {
                let v: Vec<char> = b.cur().text.chars().collect();
                let a = word_back(&v, b.col, false);
                b.delete_range(a, b.col);
                b.col = a;
            }
            KeyCode::Char('u') if ctrl => {
                let c = b.col;
                b.delete_range(0, c);
                b.col = 0;
            }
            KeyCode::Char('s') if ctrl => {
                self.mode = Mode::Normal;
                self.end_insert(b);
                b.clamp();
                return Action::Save;
            }
            KeyCode::Char(c) if !ctrl => b.insert_str(&c.to_string()),
            KeyCode::Enter => b.split_line(),
            KeyCode::Backspace => {
                if b.col > 0 {
                    let c = b.col;
                    b.delete_range(c - 1, c);
                    b.col -= 1;
                } else {
                    b.join_prev();
                }
            }
            KeyCode::Delete => {
                if b.col < b.cur().chars() {
                    let c = b.col;
                    b.delete_range(c, c + 1);
                } else {
                    b.join_next();
                }
            }
            KeyCode::Left => b.col = b.col.saturating_sub(1),
            KeyCode::Right => b.col = (b.col + 1).min(b.cur().chars()),
            KeyCode::Home => b.col = 0,
            KeyCode::End => b.col = b.cur().chars(),
            KeyCode::Up => {
                b.row = b.row.saturating_sub(1);
                b.clamp_insert();
            }
            KeyCode::Down => {
                b.row = (b.row + 1).min(b.lines.len() - 1);
                b.clamp_insert();
            }
            KeyCode::Tab => b.insert_str("  "),
            _ => {}
        }
        Action::None
    }

    // ── command line (: and /) ──────────────────────────────────────────────

    fn cmd_key(&mut self, b: &mut Buffer, k: KeyEvent) -> Action {
        let Mode::Cmd { kind, mut text, mut cur, origin } = self.mode.clone() else { return Action::None };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.unpreview(b);
                if kind == '/' {
                    (b.row, b.col) = origin;
                }
                return Action::None;
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                self.unpreview(b);
                if kind == '/' {
                    if !text.is_empty() {
                        self.search = text;
                    }
                    return Action::None;
                }
                return self.ex(b, text.trim());
            }
            KeyCode::Tab if kind == ':' => {
                (text, cur) = self.complete(b, &text, cur);
            }
            KeyCode::Backspace => {
                if text.is_empty() {
                    self.mode = Mode::Normal;
                    self.unpreview(b);
                    return Action::None;
                }
                if cur > 0 {
                    let bi = crate::buffer::byte_at(&text, cur - 1);
                    text.remove(bi);
                    cur -= 1;
                }
            }
            KeyCode::Char('u') if ctrl => {
                text.clear();
                cur = 0;
            }
            KeyCode::Left => cur = cur.saturating_sub(1),
            KeyCode::Right => cur = (cur + 1).min(text.chars().count()),
            KeyCode::Char(c) if !ctrl => {
                let bi = crate::buffer::byte_at(&text, cur);
                text.insert(bi, c);
                cur += 1;
            }
            _ => {}
        }
        if kind == '/' {
            if let Some(r) = search_from(b, &text, origin.0, true, false) {
                b.row = r;
                b.col = 0;
            } else {
                (b.row, b.col) = origin;
            }
        }
        if kind == ':' {
            self.live_substitute(b, &text);
        }
        self.mode = Mode::Cmd { kind, text, cur, origin };
        Action::None
    }

    /// Put back the lines a live `:s` preview drew over.
    fn unpreview(&mut self, b: &mut Buffer) {
        if let Some(base) = self.sub_base.take() {
            b.lines = base;
            b.clamp();
        }
    }

    /// Show what a `:s/pat/rep/` being typed would do, without committing it.
    fn live_substitute(&mut self, b: &mut Buffer, text: &str) {
        if let Some(base) = &self.sub_base {
            b.lines = base.clone();
        }
        let (range, rest) = parse_range(text.trim_start(), b, self.last_visual);
        let Some(sub) = parse_sub(rest) else {
            self.unpreview(b);
            return;
        };
        if self.sub_base.is_none() {
            self.sub_base = Some(b.lines.clone());
        }
        let (lo, hi) = range.unwrap_or((b.row, b.row));
        match sub.and_then(|s| substitute(&mut b.lines, lo, hi, &s)) {
            Ok((n, _)) if n > 0 => self.msg = Some(format!("{n} match{}", if n == 1 { "" } else { "es" })),
            Ok(_) => self.msg = Some("no match".into()),
            Err(e) => self.msg = Some(e),
        }
        b.clamp();
    }

    /// `<Tab>` in `:` mode: complete command names, and paths after `:cd` / `:e`.
    fn complete(&mut self, b: &Buffer, text: &str, cur: usize) -> (String, usize) {
        let unchanged = (text.to_string(), cur);
        if cur != text.chars().count() {
            return unchanged;
        }
        let Some((head, arg)) = text.split_once(' ') else {
            let cands: Vec<String> = EX_COMMANDS.iter().filter(|c| c.starts_with(text)).map(|c| c.to_string()).collect();
            return self.fill(text, "", &cands).map(|t| { let n = t.chars().count(); (t, n) }).unwrap_or(unchanged);
        };
        if !matches!(head, "cd" | "e" | "edit") {
            return unchanged;
        }
        let dirs_only = head == "cd";
        let home = std::env::var("HOME").unwrap_or_default();
        let (dpart, partial) = match arg.rfind('/') {
            Some(i) => (&arg[..=i], &arg[i + 1..]),
            None => ("", arg),
        };
        let base = if let Some(r) = dpart.strip_prefix('~') {
            std::path::PathBuf::from(format!("{home}{r}"))
        } else if dpart.starts_with('/') {
            std::path::PathBuf::from(dpart)
        } else {
            b.dir.join(dpart)
        };
        let mut cands: Vec<String> = std::fs::read_dir(&base)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        let is_dir = e.path().is_dir();
                        let ok = name.starts_with(partial) && (partial.starts_with('.') || !name.starts_with('.')) && (is_dir || !dirs_only);
                        ok.then(|| if is_dir { format!("{name}/") } else { name })
                    })
                    .collect()
            })
            .unwrap_or_default();
        cands.sort_by(|a, b| crate::entry::natural_cmp(a, b));
        let prefix = format!("{head} {dpart}");
        match self.fill(partial, &prefix, &cands) {
            Some(t) => {
                let n = t.chars().count();
                (t, n)
            }
            None => unchanged,
        }
    }

    /// Longest common completion of `partial` among `cands` (prefixed by
    /// `lead`); lists the candidates when there is more than one.
    fn fill(&mut self, partial: &str, lead: &str, cands: &[String]) -> Option<String> {
        if cands.is_empty() {
            self.msg = Some("no completions".into());
            return None;
        }
        let mut common = cands[0].clone();
        for c in &cands[1..] {
            let n = common.chars().zip(c.chars()).take_while(|(a, b)| a == b).count();
            common = common.chars().take(n).collect();
        }
        if cands.len() > 1 {
            let mut list = cands.iter().take(8).cloned().collect::<Vec<_>>().join("  ");
            if cands.len() > 8 {
                list.push_str(&format!("  … +{}", cands.len() - 8));
            }
            self.msg = Some(list);
        }
        let common = if common.chars().count() < partial.chars().count() { partial.to_string() } else { common };
        let trail = if cands.len() == 1 && !common.ends_with('/') && lead.is_empty() { " " } else { "" };
        Some(format!("{lead}{common}{trail}"))
    }

    fn ex(&mut self, b: &mut Buffer, cmd: &str) -> Action {
        let (range, rest) = parse_range(cmd, b, self.last_visual);
        if range.is_some() || parse_sub(rest).is_some() || ["prefix ", "suffix ", "chmod "].iter().any(|p| rest.starts_with(p)) {
            if let Some(a) = self.ex_range(b, range, rest) {
                return a;
            }
        }
        match cmd {
            "" => Action::None,
            "w" | "write" => Action::Save,
            "q" | "quit" => Action::Quit,
            "q!" | "qa!" | "quit!" => Action::ForceQuit,
            "wq" | "x" | "wqa" | "xa" => Action::SaveQuit,
            "e!" | "edit!" => Action::Discard,
            "e" | "edit" => Action::Refresh,
            "h" | "help" => Action::Help,
            "hidden" | "set hidden" | "set hidden!" => Action::ToggleHidden,
            "sort" => Action::CycleSort,
            "sort!" | "reverse" => Action::ReverseSort,
            "undo-apply" | "undo!" => Action::UndoApply,
            "edit-buffer" | "ebuf" => Action::EditBuffer,
            "send" => Action::Send(b.cur().id.into_iter().collect()),
            "details" => Action::ToggleDetails,
            "preview" => Action::TogglePreview,
            "files" => Action::Finder(FinderKind::Files),
            "grep" | "rg" => Action::Finder(FinderKind::Grep),
            _ => {
                if let Ok(n) = cmd.parse::<usize>() {
                    b.row = n.saturating_sub(1).min(b.lines.len() - 1);
                    b.clamp();
                    return Action::None;
                }
                if let Some(rest) = cmd.strip_prefix("touch ").or_else(|| cmd.strip_prefix("mkdir ")) {
                    let dir = cmd.starts_with("mkdir");
                    b.snapshot();
                    for name in rest.split_whitespace() {
                        let t = if dir && !name.ends_with('/') { format!("{name}/") } else { name.to_string() };
                        b.lines.push(Line::new(t));
                    }
                    b.lines.retain(|l| l.significant());
                    b.ensure_nonempty();
                    b.row = b.lines.len() - 1;
                    b.clamp();
                    self.msg = Some("added — :w to create".into());
                    return Action::None;
                }
                Action::Command(cmd.to_string())
            }
        }
    }

    /// Ex commands that work on a line range (`%`, `'<,'>`, `n,m`, default
    /// the cursor line): `s/pat/rep/[gi]`, `prefix`, `suffix`, `chmod`.
    fn ex_range(&mut self, b: &mut Buffer, range: Option<(usize, usize)>, rest: &str) -> Option<Action> {
        let (lo, hi) = range.unwrap_or((b.row, b.row));
        let hi = hi.min(b.lines.len() - 1);
        if let Some(sub) = parse_sub(rest) {
            let sub = match sub {
                Ok(s) => s,
                Err(e) => {
                    self.msg = Some(e);
                    return Some(Action::None);
                }
            };
            let before = b.lines.clone();
            b.snapshot();
            match substitute(&mut b.lines, lo, hi, &sub) {
                Ok((0, _)) => {
                    b.squash_noop();
                    self.msg = Some(format!("pattern not found: {}", sub.pat));
                }
                Ok((n, l)) => self.msg = Some(format!("{n} substitution{} on {l} line{} — :w to rename", if n == 1 { "" } else { "s" }, if l == 1 { "" } else { "s" })),
                Err(e) => {
                    b.lines = before;
                    b.squash_noop();
                    self.msg = Some(e);
                }
            }
            b.clamp();
            return Some(Action::None);
        }
        let (word, arg) = rest.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((rest, ""));
        match word {
            "prefix" | "suffix" if !arg.is_empty() => {
                b.snapshot();
                let mut n = 0;
                for l in b.lines[lo..=hi].iter_mut().filter(|l| l.significant()) {
                    l.text = if word == "prefix" { format!("{arg}{}", l.text) } else { with_suffix(&l.text, arg) };
                    n += 1;
                }
                self.msg = Some(format!("{word}ed {n} name{} — :w to rename", if n == 1 { "" } else { "s" }));
                Some(Action::None)
            }
            "send" => Some(Action::Send(b.lines[lo..=hi].iter().filter_map(|l| l.id).collect())),
            "chmod" if !arg.is_empty() => {
                let ids: Vec<u64> = b.lines[lo..=hi].iter().filter_map(|l| l.id).collect();
                Some(Action::Chmod { spec: arg.to_string(), ids })
            }
            "" => {
                b.row = lo;
                Some(Action::None)
            }
            _ if range.is_some() => {
                self.msg = Some(format!("not a range command: {rest}"));
                Some(Action::None)
            }
            _ => None,
        }
    }

    // ── visual line ─────────────────────────────────────────────────────────

    fn visual_key(&mut self, b: &mut Buffer, k: KeyEvent, anchor: usize) -> Action {
        let (lo, hi) = (anchor.min(b.row), anchor.max(b.row));
        match k.code {
            KeyCode::Esc | KeyCode::Char('V') | KeyCode::Char('v') => self.mode = Mode::Normal,
            KeyCode::Char('j') | KeyCode::Down => b.row = (b.row + 1).min(b.lines.len() - 1),
            KeyCode::Char('k') | KeyCode::Up => b.row = b.row.saturating_sub(1),
            KeyCode::Char('G') => b.row = b.lines.len() - 1,
            KeyCode::Char('g') => b.row = 0,
            KeyCode::Char('o') => {
                self.mode = Mode::Visual { anchor: b.row };
                b.row = anchor;
            }
            KeyCode::Char(c @ (':' | 'I' | 'A')) => {
                // `:` gets the selection as a range; I / A prefill :prefix / :suffix.
                self.last_visual = Some((lo, hi));
                let text = match c {
                    'I' => "'<,'>prefix ",
                    'A' => "'<,'>suffix ",
                    _ => "'<,'>",
                }
                .to_string();
                let cur = text.chars().count();
                self.mode = Mode::Cmd { kind: ':', text, cur, origin: (b.row, b.col) };
                return Action::None;
            }
            KeyCode::Char('d') | KeyCode::Char('x') | KeyCode::Delete => {
                b.snapshot();
                let cut: Vec<Line> = b.lines.drain(lo..=hi).collect();
                self.msg = Some(format!("{} line(s) cut — p to paste (moves)", cut.len()));
                self.set_lines(cut, true);
                b.ensure_nonempty();
                b.row = lo;
                self.mode = Mode::Normal;
            }
            KeyCode::Char('y') => {
                self.set_lines(b.lines[lo..=hi].to_vec(), false);
                self.msg = Some(format!("{} line(s) yanked — p to paste (copies)", hi - lo + 1));
                b.row = lo;
                self.mode = Mode::Normal;
            }
            KeyCode::Char('c') => {
                b.snapshot();
                let cut: Vec<Line> = b.lines.drain(lo..=hi).collect();
                self.set_lines(cut, true);
                b.lines.insert(lo.min(b.lines.len()), Line::new(""));
                b.row = lo;
                b.col = 0;
                self.mode = Mode::Insert;
                return Action::None;
            }
            KeyCode::Char('~') | KeyCode::Char('u') | KeyCode::Char('U') => {
                b.snapshot();
                let c = match k.code {
                    KeyCode::Char('u') => 'u',
                    KeyCode::Char('U') => 'U',
                    _ => '~',
                };
                for l in &mut b.lines[lo..=hi] {
                    l.text = match c {
                        'u' => l.text.to_lowercase(),
                        'U' => l.text.to_uppercase(),
                        _ => l.text.chars().map(|ch| if ch.is_uppercase() { ch.to_lowercase().next().unwrap() } else { ch.to_uppercase().next().unwrap() }).collect(),
                    };
                }
                self.mode = Mode::Normal;
            }
            _ => {}
        }
        b.clamp();
        Action::None
    }

    // ── normal ──────────────────────────────────────────────────────────────

    fn normal_key(&mut self, b: &mut Buffer, k: KeyEvent) -> Action {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        if ctrl {
            self.pending.clear();
            let half = (b.view_h / 2).max(1);
            return match k.code {
                KeyCode::Char('r') => {
                    if !b.redo() {
                        self.msg = Some("already at newest change".into());
                    }
                    Action::None
                }
                KeyCode::Char('d') => {
                    b.row = (b.row + half).min(b.lines.len() - 1);
                    b.clamp();
                    Action::None
                }
                KeyCode::Char('u') => {
                    b.row = b.row.saturating_sub(half);
                    b.clamp();
                    Action::None
                }
                KeyCode::Char('p') => Action::TogglePreview,
                KeyCode::Char('l') => Action::Refresh,
                KeyCode::Char('s') => Action::Save,
                KeyCode::Char('v') => Action::Open(Some(OpenTarget::SplitRight)),
                KeyCode::Char('x') => Action::Open(Some(OpenTarget::SplitDown)),
                KeyCode::Char('t') => Action::Open(Some(OpenTarget::Tab)),
                KeyCode::Char('e') => Action::Open(Some(OpenTarget::Popup)),
                KeyCode::Char('f') => Action::PreviewScroll(half as i32),
                KeyCode::Char('b') => Action::PreviewScroll(-(half as i32)),
                KeyCode::Char('c') => Action::Quit,
                _ => Action::None,
            };
        }
        if alt {
            return Action::None;
        }
        let ch = match k.code {
            KeyCode::Char(c) => c,
            KeyCode::Enter => '\n',
            KeyCode::Esc => {
                // First cancel a half-typed command; Esc on a clean slate closes
                // the popup (the app asks first when there are unsaved edits).
                if self.pending.is_empty() {
                    return Action::Quit;
                }
                self.pending.clear();
                return Action::None;
            }
            KeyCode::Backspace => {
                self.pending.clear();
                return Action::Parent;
            }
            KeyCode::Tab => {
                self.pending.clear();
                return Action::Finder(FinderKind::Files);
            }
            KeyCode::Up => 'k',
            KeyCode::Down => 'j',
            KeyCode::Left => 'h',
            KeyCode::Right => 'l',
            KeyCode::Home => '0',
            KeyCode::End => '$',
            KeyCode::Delete => 'x',
            KeyCode::PageDown => {
                b.row = (b.row + b.view_h.max(1)).min(b.lines.len() - 1);
                b.clamp();
                return Action::None;
            }
            KeyCode::PageUp => {
                b.row = b.row.saturating_sub(b.view_h.max(1));
                b.clamp();
                return Action::None;
            }
            _ => return Action::None,
        };
        if ch == '.' && self.pending.iter().all(|c| c.is_ascii_digit()) {
            let n: usize = self.pending.iter().collect::<String>().parse().unwrap_or(1);
            self.pending.clear();
            let keys = self.last_change.clone();
            if keys.is_empty() {
                self.msg = Some("nothing to repeat".into());
            }
            for _ in 0..n.clamp(1, 100) {
                for k in &keys {
                    self.handle(b, *k);
                }
            }
            return Action::None;
        }
        if self.pending.is_empty() {
            self.cmd_keys.clear();
        }
        self.cmd_keys.push(k);
        let edits = b.edits;
        self.pending.push(ch);
        let keys = self.pending.clone();
        let mut act = Action::None;
        match self.run(b, &keys, &mut act) {
            Parse::More => {}
            Parse::Bad => self.pending.clear(),
            Parse::Done => {
                self.pending.clear();
                if self.mode == Mode::Insert {
                    self.recording = Some(self.cmd_keys.clone());
                } else if b.edits != edits {
                    self.last_change = self.cmd_keys.clone();
                }
            }
        }
        if self.mode == Mode::Normal {
            b.clamp();
        }
        act
    }

    fn run(&mut self, b: &mut Buffer, keys: &[char], act: &mut Action) -> Parse {
        let mut i = 0;
        let count1 = take_count(keys, &mut i);
        let Some(&c) = keys.get(i) else { return Parse::More };
        i += 1;
        let n = count1.unwrap_or(1);
        let last = b.lines.len() - 1;
        match c {
            // plain motions
            'h' | 'l' | 'w' | 'b' | 'e' | 'W' | 'B' | 'E' | '0' | '^' | '$' | 'j' | 'k' | 'G' | 'f' | 'F' | 't' | 'T' | ';' | ',' => {
                if matches!(c, 'f' | 'F' | 't' | 'T') && keys.get(i).is_none() {
                    return Parse::More;
                }
                match self.motion(b, &keys[i - 1..], n, count1.is_some(), None) {
                    Err(p) => return p,
                    Ok((m, _)) => match m {
                        Motion::Char { to, .. } => b.col = to,
                        Motion::Range { a, .. } => b.col = a,
                        Motion::Lines { to } => {
                            b.row = to;
                            if c == 'G' {
                                b.col = 0;
                            }
                        }
                        Motion::LinesAbs { a, .. } => b.row = a,
                    },
                }
                Parse::Done
            }
            'g' => {
                let Some(&d) = keys.get(i) else { return Parse::More };
                match d {
                    'g' => {
                        b.row = count1.map(|c| c.saturating_sub(1)).unwrap_or(0).min(last);
                        b.col = 0;
                    }
                    '.' => *act = Action::ToggleHidden,
                    's' => *act = Action::CycleSort,
                    'S' => *act = Action::ReverseSort,
                    'e' => *act = Action::EditBuffer,
                    'i' => *act = Action::Send(b.cur().id.into_iter().collect()),
                    'd' => *act = Action::ToggleDetails,
                    '?' => *act = Action::Help,
                    'x' => *act = Action::Open(Some(OpenTarget::System)),
                    'y' => *act = Action::YankPath(PathKind::Relative),
                    'Y' => *act = Action::YankPath(PathKind::Absolute),
                    'n' => *act = Action::YankPath(PathKind::Name),
                    'p' => *act = Action::TogglePreview,
                    'm' | 'c' => {
                        if self.reg_entries() == 0 {
                            self.msg = Some("yank (yy) or cut (dd) a file or folder first, then gm / gc".into());
                        } else if d == 'c' && self.reg_cut {
                            self.msg = Some("cut lines can only be moved — gm (or yy to copy)".into());
                        } else {
                            *act = Action::Transfer { copy: d == 'c' };
                        }
                    }
                    'f' => *act = Action::Finder(FinderKind::Files),
                    '/' => *act = Action::Finder(FinderKind::Grep),
                    _ => return Parse::Bad,
                }
                Parse::Done
            }
            ' ' => {
                let Some(&d) = keys.get(i) else { return Parse::More };
                *act = match d {
                    'f' | ' ' => Action::Finder(FinderKind::Files),
                    'g' | '/' => Action::Finder(FinderKind::Grep),
                    'd' => Action::Finder(FinderKind::Dirs),
                    'r' => Action::Finder(FinderKind::Recent),
                    's' => Action::Finder(FinderKind::Git),
                    _ => return Parse::Bad,
                };
                Parse::Done
            }
            'Z' => {
                let Some(&d) = keys.get(i) else { return Parse::More };
                *act = match d {
                    'Z' => Action::SaveQuit,
                    'Q' => Action::ForceQuit,
                    _ => return Parse::Bad,
                };
                Parse::Done
            }
            '\n' => {
                *act = Action::Open(None);
                Parse::Done
            }
            '-' => {
                *act = Action::Parent;
                Parse::Done
            }
            '_' => {
                *act = Action::StartDir;
                Parse::Done
            }
            'H' => {
                *act = Action::HistBack;
                Parse::Done
            }
            'L' => {
                *act = Action::HistFwd;
                Parse::Done
            }
            'm' | '\'' | '`' => {
                let Some(&d) = keys.get(i) else { return Parse::More };
                if !d.is_ascii_alphabetic() {
                    return Parse::Bad;
                }
                *act = if c == 'm' { Action::SetMark(d) } else { Action::JumpMark(d) };
                Parse::Done
            }
            '~' => {
                *act = Action::Home;
                Parse::Done
            }
            'q' => {
                *act = Action::Quit;
                Parse::Done
            }
            ':' => {
                self.mode = Mode::Cmd { kind: ':', text: String::new(), cur: 0, origin: (b.row, b.col) };
                Parse::Done
            }
            '/' | '?' => {
                self.mode = Mode::Cmd { kind: '/', text: String::new(), cur: 0, origin: (b.row, b.col) };
                Parse::Done
            }
            'n' | 'N' => {
                if self.search.is_empty() {
                    self.msg = Some("no previous search".into());
                } else if let Some(r) = search_from(b, &self.search.clone(), b.row, c == 'n', false) {
                    b.row = r;
                    b.col = 0;
                } else {
                    self.msg = Some(format!("pattern not found: {}", self.search));
                }
                Parse::Done
            }
            'U' => {
                *act = Action::UndoApply;
                Parse::Done
            }
            'u' => {
                if !b.undo() {
                    self.msg = Some("already at oldest change".into());
                }
                Parse::Done
            }
            'V' | 'v' => {
                self.mode = Mode::Visual { anchor: b.row };
                Parse::Done
            }
            'i' | 'a' | 'I' | 'A' => {
                b.snapshot();
                b.col = match c {
                    'i' => b.col,
                    'a' => (b.col + 1).min(b.cur().chars()),
                    'I' => 0,
                    _ => b.cur().chars(),
                };
                self.mode = Mode::Insert;
                Parse::Done
            }
            'o' | 'O' => {
                b.snapshot();
                let at = if c == 'o' { b.row + 1 } else { b.row };
                let at = if b.lines.len() == 1 && !b.lines[0].significant() { 0 } else { at };
                if b.lines.len() == 1 && !b.lines[0].significant() {
                    b.lines[0] = Line::new("");
                } else {
                    b.lines.insert(at, Line::new(""));
                }
                b.row = at;
                b.col = 0;
                self.mode = Mode::Insert;
                Parse::Done
            }
            'x' | 'X' | 's' => {
                let len = b.cur().chars();
                if len == 0 {
                    if c == 's' {
                        b.snapshot();
                        self.mode = Mode::Insert;
                    }
                    return Parse::Done;
                }
                b.snapshot();
                let (a, e) = if c == 'X' { (b.col.saturating_sub(n), b.col) } else { (b.col, (b.col + n).min(len)) };
                let cut = b.delete_range(a, e);
                self.reg = Some(Reg::Chars(cut));
                b.col = a;
                if c == 's' {
                    self.mode = Mode::Insert;
                }
                Parse::Done
            }
            'D' | 'C' => {
                b.snapshot();
                let len = b.cur().chars();
                let col = b.col;
                let cut = b.delete_range(col, len);
                self.reg = Some(Reg::Chars(cut));
                if c == 'C' {
                    self.mode = Mode::Insert;
                }
                Parse::Done
            }
            'S' => {
                b.snapshot();
                b.cur_mut().text.clear();
                b.col = 0;
                self.mode = Mode::Insert;
                Parse::Done
            }
            'Y' => {
                let e = (b.row + n - 1).min(last);
                self.set_lines(b.lines[b.row..=e].to_vec(), false);
                Parse::Done
            }
            'r' => {
                let Some(&d) = keys.get(i) else { return Parse::More };
                if b.cur().chars() == 0 || d == '\n' {
                    return Parse::Done;
                }
                b.snapshot();
                let col = b.col;
                let end = (col + n).min(b.cur().chars());
                b.delete_range(col, end);
                let s: String = std::iter::repeat_n(d, end - col).collect();
                b.col = col;
                b.insert_str(&s);
                b.col = end - 1;
                Parse::Done
            }
            'J' => {
                b.snapshot();
                for _ in 0..n.max(1) {
                    b.join_next();
                }
                Parse::Done
            }
            'p' | 'P' => {
                let Some(reg) = self.reg.clone() else {
                    self.msg = Some("nothing to paste".into());
                    return Parse::Done;
                };
                b.snapshot();
                match reg {
                    Reg::Lines(ls) => {
                        let blank = b.lines.len() == 1 && !b.lines[0].significant();
                        let mut at = if c == 'p' { b.row + 1 } else { b.row };
                        if blank {
                            b.lines.clear();
                            at = 0;
                        }
                        let mut ins = Vec::new();
                        for _ in 0..n {
                            ins.extend(ls.iter().cloned());
                        }
                        let k = ins.len();
                        b.lines.splice(at..at, ins);
                        b.row = at;
                        b.col = 0;
                        self.msg = Some(format!("{k} line(s) pasted"));
                    }
                    Reg::Chars(s) => {
                        if c == 'p' && b.cur().chars() > 0 {
                            b.col += 1;
                        }
                        let s = s.repeat(n);
                        b.insert_str(&s);
                        b.col = b.col.saturating_sub(1);
                    }
                }
                Parse::Done
            }
            'd' | 'c' | 'y' => self.operator(b, c, keys, i, count1),
            _ => Parse::Bad,
        }
    }

    fn operator(&mut self, b: &mut Buffer, op: char, keys: &[char], mut i: usize, count1: Option<usize>) -> Parse {
        let count2 = take_count(keys, &mut i);
        let Some(&m) = keys.get(i) else { return Parse::More };
        let n = count1.unwrap_or(1) * count2.unwrap_or(1);
        let counted = count1.is_some() || count2.is_some();
        let last = b.lines.len() - 1;
        let motion = if m == op {
            Motion::LinesAbs { a: b.row, b: (b.row + n - 1).min(last) }
        } else {
            if matches!(m, 'i' | 'a' | 'f' | 'F' | 't' | 'T' | 'g') && keys.get(i + 1).is_none() {
                return Parse::More;
            }
            match self.motion(b, &keys[i..], n, counted, Some(op)) {
                Ok((m, _)) => m,
                Err(p) => return p,
            }
        };
        let (lo, hi, linewise) = match motion {
            Motion::LinesAbs { a, b: e } => (a, e, true),
            Motion::Lines { to } => (b.row.min(to), b.row.max(to), true),
            Motion::Char { to, inclusive } => {
                let (a, e) = if to < b.col { (to, b.col) } else { (b.col, if inclusive { to + 1 } else { to }) };
                (a, e.min(b.cur().chars()), false)
            }
            Motion::Range { a, b: e } => (a, e, false),
        };
        if linewise {
            match op {
                'y' => {
                    self.set_lines(b.lines[lo..=hi].to_vec(), false);
                    self.msg = Some(format!("{} line(s) yanked", hi - lo + 1));
                }
                'd' => {
                    b.snapshot();
                    let cut: Vec<Line> = b.lines.drain(lo..=hi).collect();
                    self.msg = Some(format!("{} line(s) cut — p to paste (moves)", cut.len()));
                    self.set_lines(cut, true);
                    b.ensure_nonempty();
                    b.row = lo;
                }
                _ => {
                    b.snapshot();
                    if lo == hi {
                        // cc keeps the id: retyping the name renames.
                        b.row = lo;
                        b.cur_mut().text.clear();
                    } else {
                        let cut: Vec<Line> = b.lines.drain(lo..=hi).collect();
                        self.set_lines(cut, true);
                        b.lines.insert(lo.min(b.lines.len()), Line::new(""));
                        b.row = lo;
                    }
                    b.col = 0;
                    self.mode = Mode::Insert;
                }
            }
            return Parse::Done;
        }
        if lo >= hi && op != 'c' {
            return Parse::Done;
        }
        match op {
            'y' => {
                let v: Vec<char> = b.cur().text.chars().collect();
                self.reg = Some(Reg::Chars(v[lo..hi].iter().collect()));
                b.col = lo;
            }
            _ => {
                b.snapshot();
                let cut = b.delete_range(lo, hi);
                self.reg = Some(Reg::Chars(cut));
                b.col = lo;
                if op == 'c' {
                    self.mode = Mode::Insert;
                }
            }
        }
        Parse::Done
    }

    /// Parse one motion at the start of `keys`.
    fn motion(&mut self, b: &Buffer, keys: &[char], n: usize, counted: bool, op: Option<char>) -> Result<(Motion, usize), Parse> {
        let v: Vec<char> = b.cur().text.chars().collect();
        let col = b.col;
        let last = b.lines.len() - 1;
        let c = keys[0];
        let m = match c {
            'h' => Motion::Char { to: col.saturating_sub(n), inclusive: false },
            'l' => {
                let max = if op.is_some() { v.len() } else { v.len().saturating_sub(1) };
                Motion::Char { to: (col + n).min(max), inclusive: false }
            }
            'w' | 'W' => {
                let big = c == 'W';
                if op == Some('c') && v.get(col).is_some_and(|ch| !ch.is_whitespace()) {
                    // cw behaves like ce
                    let mut p = col;
                    for k in 0..n {
                        p = if k == 0 && v.get(col + 1).is_some_and(|x| cls(*x, big) != cls(v[col], big)) { col } else { word_end(&v, p, big) };
                    }
                    Motion::Char { to: p, inclusive: true }
                } else {
                    let mut p = col;
                    for _ in 0..n {
                        p = next_word_start(&v, p, big);
                    }
                    if op.is_none() {
                        p = p.min(v.len().saturating_sub(1));
                    }
                    Motion::Char { to: p, inclusive: false }
                }
            }
            'e' | 'E' => {
                let mut p = col;
                for _ in 0..n {
                    p = word_end(&v, p, c == 'E');
                }
                Motion::Char { to: p, inclusive: true }
            }
            'b' | 'B' => {
                let mut p = col;
                for _ in 0..n {
                    p = word_back(&v, p, c == 'B');
                }
                Motion::Char { to: p, inclusive: false }
            }
            '0' => Motion::Char { to: 0, inclusive: false },
            '^' => Motion::Char { to: v.iter().position(|c| !c.is_whitespace()).unwrap_or(0), inclusive: false },
            '$' => {
                if op.is_some() {
                    Motion::Range { a: col, b: v.len() }
                } else {
                    Motion::Char { to: v.len().saturating_sub(1), inclusive: true }
                }
            }
            'j' => Motion::Lines { to: (b.row + n).min(last) },
            'k' => Motion::Lines { to: b.row.saturating_sub(n) },
            'G' => Motion::Lines { to: if counted { n.saturating_sub(1).min(last) } else { last } },
            'g' => match keys.get(1) {
                Some('g') => Motion::Lines { to: if counted { n.saturating_sub(1).min(last) } else { 0 } },
                Some(_) => return Err(Parse::Bad),
                None => return Err(Parse::More),
            },
            'f' | 'F' | 't' | 'T' => {
                let Some(&ch) = keys.get(1) else { return Err(Parse::More) };
                self.search_char = Some((c, ch));
                match find_char(&v, col, c, ch, n) {
                    Some(p) => Motion::Char { to: p, inclusive: matches!(c, 'f' | 't') },
                    None => return Err(Parse::Bad),
                }
            }
            ';' | ',' => {
                let Some((k, ch)) = self.search_char else { return Err(Parse::Bad) };
                let k = if c == ',' {
                    match k {
                        'f' => 'F',
                        'F' => 'f',
                        't' => 'T',
                        _ => 't',
                    }
                } else {
                    k
                };
                match find_char(&v, col, k, ch, n) {
                    Some(p) => Motion::Char { to: p, inclusive: matches!(k, 'f' | 't') },
                    None => return Err(Parse::Bad),
                }
            }
            'i' | 'a' if op.is_some() => {
                let Some(&o) = keys.get(1) else { return Err(Parse::More) };
                let (a, e) = match o {
                    'w' => word_object(&v, col, c == 'a', false),
                    'W' => word_object(&v, col, c == 'a', true),
                    // stop at ` -> target` on symlink lines
                    'n' => name_object(&v[..crate::plan::split_link(&b.cur().text).0.chars().count()], c == 'a'),
                    _ => return Err(Parse::Bad),
                };
                Motion::Range { a, b: e }
            }
            _ => return Err(Parse::Bad),
        };
        Ok((m, 1))
    }
}

/// Ex command names offered by `<Tab>` completion.
const EX_COMMANDS: &[&str] = &[
    "cd", "chmod", "details", "edit", "edit-buffer", "files", "grep", "help", "hidden", "marks", "mkdir", "prefix", "preview", "pwd", "quit", "reverse", "send", "sort", "suffix", "touch", "undo-apply", "write",
];

/// A parsed `s/pat/rep/flags`.
pub struct Sub {
    pub pat: String,
    re: regex::Regex,
    rep: String,
    global: bool,
}

/// Split a leading line range off an ex command: `%`, `'<,'>`, `n,m`, `.`.
fn parse_range<'a>(cmd: &'a str, b: &Buffer, visual: Option<(usize, usize)>) -> (Option<(usize, usize)>, &'a str) {
    let last = b.lines.len() - 1;
    if let Some(r) = cmd.strip_prefix('%') {
        return (Some((0, last)), r.trim_start());
    }
    if let Some(r) = cmd.strip_prefix("'<,'>") {
        let (a, z) = visual.unwrap_or((b.row, b.row));
        return (Some((a.min(last), z.min(last))), r.trim_start());
    }
    let num = |s: &str| -> Option<usize> {
        if s == "." {
            Some(b.row)
        } else if s == "$" {
            Some(last)
        } else {
            s.parse::<usize>().ok().map(|n| n.saturating_sub(1).min(last))
        }
    };
    let end = cmd.find(|c: char| !(c.is_ascii_digit() || c == ',' || c == '.' || c == '$')).unwrap_or(cmd.len());
    if end == 0 || end == cmd.len() {
        return (None, cmd);
    }
    let (r, rest) = cmd.split_at(end);
    let range = match r.split_once(',') {
        Some((a, z)) => num(a).zip(num(z)).map(|(a, z)| (a.min(z), a.max(z))),
        None => num(r).map(|a| (a, a)),
    };
    match range {
        Some(rg) => (Some(rg), rest.trim_start()),
        None => (None, cmd),
    }
}

/// `s/pat/rep/[gi]` with any non-alphanumeric delimiter. `None` when the
/// text is not a substitution at all, `Some(Err)` when it is malformed.
fn parse_sub(cmd: &str) -> Option<Result<Sub, String>> {
    let rest = cmd.strip_prefix("s")?;
    let delim = rest.chars().next().filter(|c| !c.is_alphanumeric() && !c.is_whitespace() && *c != '\\')?;
    let mut parts: Vec<String> = vec![String::new()];
    let mut esc = false;
    for c in rest[delim.len_utf8()..].chars() {
        if esc {
            if c != delim {
                parts.last_mut().unwrap().push('\\');
            }
            parts.last_mut().unwrap().push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == delim && parts.len() < 3 {
            parts.push(String::new());
        } else {
            parts.last_mut().unwrap().push(c);
        }
    }
    let pat = parts[0].clone();
    if pat.is_empty() {
        return Some(Err("empty pattern".into()));
    }
    let flags = parts.get(2).cloned().unwrap_or_default();
    let re = regex::RegexBuilder::new(&pat).case_insensitive(flags.contains('i')).build();
    let re = match re {
        Ok(r) => r,
        Err(_) => return Some(Err(format!("bad pattern: {pat}"))),
    };
    // vim-style \1 groups → regex ${1}
    let mut rep = String::new();
    let raw = parts.get(1).cloned().unwrap_or_default();
    let mut it = raw.chars().peekable();
    while let Some(c) = it.next() {
        match (c, it.peek()) {
            ('\\', Some(d)) if d.is_ascii_digit() => {
                rep.push_str(&format!("${{{d}}}"));
                it.next();
            }
            _ => rep.push(c),
        }
    }
    Some(Ok(Sub { pat, re, rep, global: flags.contains('g') }))
}

/// Apply `sub` to lines lo..=hi; returns (substitutions, lines changed).
fn substitute(lines: &mut [Line], lo: usize, hi: usize, sub: &Sub) -> Result<(usize, usize), String> {
    let (mut n, mut changed) = (0, 0);
    let hi = hi.min(lines.len().saturating_sub(1));
    for l in lines[lo..=hi].iter_mut().filter(|l| l.significant()) {
        let hits = sub.re.find_iter(&l.text).count();
        if hits == 0 {
            continue;
        }
        let new = if sub.global { sub.re.replace_all(&l.text, sub.rep.as_str()) } else { sub.re.replace(&l.text, sub.rep.as_str()) };
        n += if sub.global { hits } else { 1 };
        if new != l.text {
            changed += 1;
            l.text = new.into_owned();
        }
    }
    Ok((n, changed))
}

/// Append to a name's stem: `report.md` + `-v2` → `report-v2.md`, `dir/` → `dir-v2/`.
fn with_suffix(text: &str, suf: &str) -> String {
    if let (name, Some(t)) = crate::plan::split_link(text) {
        return format!("{} -> {t}", with_suffix(name, suf));
    }
    let slash = text.ends_with('/');
    let core = text.trim_end_matches('/');
    let v: Vec<char> = core.chars().collect();
    let (stem, ext) = if slash {
        (core.to_string(), String::new())
    } else {
        let (_, d) = name_object(&v, false);
        (v[..d].iter().collect(), v[d..].iter().collect())
    };
    format!("{stem}{suf}{ext}{}", if slash { "/" } else { "" })
}

fn take_count(keys: &[char], i: &mut usize) -> Option<usize> {
    let start = *i;
    while let Some(c) = keys.get(*i) {
        if c.is_ascii_digit() && !(*i == start && *c == '0') {
            *i += 1;
        } else {
            break;
        }
    }
    if *i == start {
        None
    } else {
        keys[start..*i].iter().collect::<String>().parse().ok()
    }
}

/// Smart-case substring search over lines, starting after/before `from`.
fn search_from(b: &Buffer, pat: &str, from: usize, forward: bool, include_from: bool) -> Option<usize> {
    if pat.is_empty() {
        return None;
    }
    let smart = pat.chars().any(|c| c.is_uppercase());
    let hit = |t: &str| if smart { t.contains(pat) } else { t.to_lowercase().contains(&pat.to_lowercase()) };
    let n = b.lines.len();
    for k in 0..n {
        let off = if include_from { k } else { k + 1 };
        let r = if forward { (from + off) % n } else { (from + n * 2 - off % n) % n };
        if hit(&b.lines[r].text) {
            return Some(r);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Line;
    use std::path::PathBuf;

    fn buf(lines: &[(Option<u64>, &str)]) -> Buffer {
        let mut store = crate::entry::Store::default();
        let t = std::env::temp_dir().join(format!("herdr-oil-vim-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&t);
        let mut b = Buffer::load(&t, &mut store, false, crate::entry::Sort::Name, false);
        b.lines = lines.iter().map(|(id, t)| Line { id: *id, text: t.to_string() }).collect();
        b.dir = PathBuf::from("/x");
        b
    }

    fn keys(v: &mut Vim, b: &mut Buffer, s: &str) -> Vec<Action> {
        let mut out = vec![];
        for c in s.chars() {
            let code = match c {
                '\x1b' => KeyCode::Esc,
                '\n' => KeyCode::Enter,
                c => KeyCode::Char(c),
            };
            let a = v.handle(b, KeyEvent::new(code, KeyModifiers::NONE));
            if a != Action::None {
                out.push(a);
            }
        }
        out
    }

    #[test]
    fn dd_p_moves_line_with_id() {
        let mut b = buf(&[(Some(1), "a"), (Some(2), "b"), (Some(3), "c")]);
        let mut v = Vim::default();
        keys(&mut v, &mut b, "ddp");
        let ids: Vec<_> = b.lines.iter().map(|l| l.id).collect();
        assert_eq!(ids, [Some(2), Some(1), Some(3)]);
    }

    #[test]
    fn yy_p_duplicates_id() {
        let mut b = buf(&[(Some(1), "a")]);
        let mut v = Vim::default();
        keys(&mut v, &mut b, "yyp");
        assert_eq!(b.lines.len(), 2);
        assert_eq!(b.lines[1].id, Some(1));
    }

    #[test]
    fn ciw_and_cin_rename() {
        let mut b = buf(&[(Some(1), "report-draft.md")]);
        let mut v = Vim::default();
        keys(&mut v, &mut b, "cinfinal\x1b");
        assert_eq!(b.lines[0].text, "final.md");
        assert_eq!(b.lines[0].id, Some(1));
        keys(&mut v, &mut b, "0cwreport\x1b");
        assert_eq!(b.lines[0].text, "report.md");
        keys(&mut v, &mut b, "u");
        assert_eq!(b.lines[0].text, "final.md");
    }

    #[test]
    fn o_creates_line_without_id_and_counts_work() {
        let mut b = buf(&[(Some(1), "a"), (Some(2), "b"), (Some(3), "c"), (Some(4), "d")]);
        let mut v = Vim::default();
        keys(&mut v, &mut b, "onew.txt\x1b");
        assert_eq!(b.lines[1], Line::new("new.txt"));
        keys(&mut v, &mut b, "gg2dd");
        assert_eq!(b.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["b", "c", "d"]);
        keys(&mut v, &mut b, "G");
        assert_eq!(b.row, 2);
        keys(&mut v, &mut b, "dgg");
        assert_eq!(b.lines.len(), 1);
    }

    #[test]
    fn actions_and_ex() {
        let mut b = buf(&[(Some(1), "a")]);
        let mut v = Vim::default();
        assert_eq!(keys(&mut v, &mut b, "-"), [Action::Parent]);
        assert_eq!(keys(&mut v, &mut b, "gm"), []); // nothing yanked yet
        assert_eq!(keys(&mut v, &mut b, "yygm"), [Action::Transfer { copy: false }]);
        assert_eq!(keys(&mut v, &mut b, "gc"), [Action::Transfer { copy: true }]);
        assert_eq!(keys(&mut v, &mut b, "d\x1b"), []);
        assert_eq!(keys(&mut v, &mut b, "\x1b"), [Action::Quit]);
        assert_eq!(keys(&mut v, &mut b, "ix\x1b"), []);
        assert_eq!(keys(&mut v, &mut b, "g."), [Action::ToggleHidden]);
        assert_eq!(keys(&mut v, &mut b, ":w\n"), [Action::Save]);
        assert_eq!(keys(&mut v, &mut b, ":wq\n"), [Action::SaveQuit]);
        assert_eq!(keys(&mut v, &mut b, " f"), [Action::Finder(FinderKind::Files)]);
        assert_eq!(keys(&mut v, &mut b, "\n"), [Action::Open(None)]);
        keys(&mut v, &mut b, ":touch x y/\n");
        assert_eq!(b.lines.len(), 3);
    }

    #[test]
    fn search_jumps() {
        let mut b = buf(&[(Some(1), "alpha"), (Some(2), "beta"), (Some(3), "gamma")]);
        let mut v = Vim::default();
        keys(&mut v, &mut b, "/gam\n");
        assert_eq!(b.row, 2);
        keys(&mut v, &mut b, "/a\nn");
        assert_eq!(b.row, 1);
    }

    #[test]
    fn insert_enter_splits_into_new_entry() {
        let mut b = buf(&[(Some(1), "ab")]);
        let mut v = Vim::default();
        keys(&mut v, &mut b, "a\n\x1b");
        assert_eq!(b.lines[0].id, Some(1));
        assert_eq!(b.lines[1].id, None);
        assert_eq!(b.lines[1].text, "b");
    }

    #[test]
    fn find_char_motion() {
        let mut b = buf(&[(Some(1), "one.two.three")]);
        let mut v = Vim::default();
        keys(&mut v, &mut b, "f.");
        assert_eq!(b.col, 3);
        keys(&mut v, &mut b, ";");
        assert_eq!(b.col, 7);
        keys(&mut v, &mut b, "0dt.");
        assert_eq!(b.lines[0].text, ".two.three");
    }

    fn texts(b: &Buffer) -> Vec<&str> {
        b.lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn substitute_whole_buffer_keeps_ids() {
        let mut b = buf(&[(Some(1), "IMG_1.jpeg"), (Some(2), "IMG_2.jpeg"), (Some(3), "notes.txt")]);
        let mut v = Vim::default();
        keys(&mut v, &mut b, ":%s/IMG_(\\d+)\\.jpeg/photo_$1.jpg/\n");
        assert_eq!(texts(&b), ["photo_1.jpg", "photo_2.jpg", "notes.txt"]);
        assert_eq!(b.lines[1].id, Some(2));
        keys(&mut v, &mut b, "u");
        assert_eq!(texts(&b), ["IMG_1.jpeg", "IMG_2.jpeg", "notes.txt"]);
        // vim-style groups, other delimiter, current line only
        keys(&mut v, &mut b, ":s#IMG_\\(.\\)#pic\\1#\n");
        assert_eq!(texts(&b), ["IMG_1.jpeg", "IMG_2.jpeg", "notes.txt"], "no match on line 1 without groups syntax");
        keys(&mut v, &mut b, ":s#IMG_(.)#pic\\1#\n");
        assert_eq!(texts(&b)[0], "pic1.jpeg");
    }

    #[test]
    fn live_substitute_preview_restores_on_esc() {
        let mut b = buf(&[(Some(1), "a-draft.md"), (Some(2), "b-draft.md")]);
        let mut v = Vim::default();
        keys(&mut v, &mut b, ":%s/-draft//");
        assert_eq!(texts(&b), ["a.md", "b.md"]);
        assert!(!b.undo(), "preview must not create undo steps");
        keys(&mut v, &mut b, "\x1b");
        assert_eq!(texts(&b), ["a-draft.md", "b-draft.md"]);
    }

    #[test]
    fn visual_prefix_suffix_and_ranges() {
        let mut b = buf(&[(Some(1), "a.md"), (Some(2), "b.md"), (Some(3), "dir/")]);
        let mut v = Vim::default();
        keys(&mut v, &mut b, "VjIx-\n");
        assert_eq!(texts(&b), ["x-a.md", "x-b.md", "dir/"]);
        keys(&mut v, &mut b, ":2,3suffix _v2\n");
        assert_eq!(texts(&b), ["x-a.md", "x-b_v2.md", "dir_v2/"]);
        assert_eq!(keys(&mut v, &mut b, ":%chmod +x\n"), [Action::Chmod { spec: "+x".into(), ids: vec![1, 2, 3] }]);
    }

    #[test]
    fn dot_repeats_last_change() {
        let mut b = buf(&[(Some(1), "a.txt"), (Some(2), "b.txt"), (Some(3), "c.txt")]);
        let mut v = Vim::default();
        keys(&mut v, &mut b, "cinnew\x1bj.j.");
        assert_eq!(texts(&b), ["new.txt", "new.txt", "new.txt"]);
        let mut b = buf(&[(Some(1), "a"), (Some(2), "b"), (Some(3), "c")]);
        keys(&mut v, &mut b, "dd.");
        assert_eq!(texts(&b), ["c"]);
    }

    #[test]
    fn comma_reverses_char_find() {
        let mut b = buf(&[(Some(1), "one.two.three")]);
        let mut v = Vim::default();
        keys(&mut v, &mut b, "$F.");
        assert_eq!(b.col, 7);
        keys(&mut v, &mut b, ",");
        assert_eq!(b.col, 7);
        keys(&mut v, &mut b, ";");
        assert_eq!(b.col, 3);
        keys(&mut v, &mut b, ",");
        assert_eq!(b.col, 7);
    }

    #[test]
    fn marks_history_and_yank_keys() {
        let mut b = buf(&[(Some(1), "a")]);
        let mut v = Vim::default();
        assert_eq!(keys(&mut v, &mut b, "ma'a`bHL"), [Action::SetMark('a'), Action::JumpMark('a'), Action::JumpMark('b'), Action::HistBack, Action::HistFwd]);
        assert_eq!(keys(&mut v, &mut b, "gygYgngS"), [
            Action::YankPath(PathKind::Relative),
            Action::YankPath(PathKind::Absolute),
            Action::YankPath(PathKind::Name),
            Action::ReverseSort
        ]);
    }

    #[test]
    fn tab_completes_commands() {
        let mut b = buf(&[(Some(1), "a")]);
        let mut v = Vim::default();
        keys(&mut v, &mut b, ":pre");
        v.handle(&mut b, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let Mode::Cmd { text, .. } = &v.mode else { panic!() };
        assert_eq!(text, "pre");
        assert!(v.msg.as_deref().unwrap_or("").contains("prefix"));
        keys(&mut v, &mut b, "f");
        v.handle(&mut b, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let Mode::Cmd { text, cur, .. } = &v.mode else { panic!() };
        assert_eq!((text.as_str(), *cur), ("prefix ", 7));
    }

    #[test]
    fn apply_undo_edit_and_send_keys() {
        let mut b = buf(&[(Some(7), "a"), (Some(8), "b")]);
        let mut v = Vim::default();
        assert_eq!(keys(&mut v, &mut b, "Ugegi"), [Action::UndoApply, Action::EditBuffer, Action::Send(vec![7])]);
        assert_eq!(keys(&mut v, &mut b, ":%send\n"), [Action::Send(vec![7, 8])]);
        assert_eq!(keys(&mut v, &mut b, " s"), [Action::Finder(FinderKind::Git)]);
    }

    #[test]
    fn cin_stops_before_link_target() {
        let mut b = buf(&[(Some(1), "current.toml -> config.dev.toml")]);
        let mut v = Vim::default();
        keys(&mut v, &mut b, "cinlive\x1b");
        assert_eq!(b.lines[0].text, "live.toml -> config.dev.toml");
    }
}
