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
    YankPath,
    Command(String),
    PreviewScroll(i32),
    /// Move (`copy: false`) or copy the yanked/cut lines to a folder picked
    /// with the fuzzy finder.
    Transfer { copy: bool },
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
        Vim { mode: Mode::Normal, pending: vec![], reg: None, search: String::new(), search_char: None, reg_cut: false, msg: None }
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

    fn insert_key(&mut self, b: &mut Buffer, k: KeyEvent) -> Action {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                b.col = b.col.saturating_sub(1);
                b.squash_noop();
                b.clamp();
            }
            KeyCode::Char('c') if ctrl => {
                self.mode = Mode::Normal;
                b.squash_noop();
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
                b.squash_noop();
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
                if kind == '/' {
                    (b.row, b.col) = origin;
                }
                return Action::None;
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                if kind == '/' {
                    if !text.is_empty() {
                        self.search = text;
                    }
                    return Action::None;
                }
                return self.ex(b, text.trim());
            }
            KeyCode::Backspace => {
                if text.is_empty() {
                    self.mode = Mode::Normal;
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
        self.mode = Mode::Cmd { kind, text, cur, origin };
        Action::None
    }

    fn ex(&mut self, b: &mut Buffer, cmd: &str) -> Action {
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
        self.pending.push(ch);
        let keys = self.pending.clone();
        let mut act = Action::None;
        match self.run(b, &keys, &mut act) {
            Parse::More => {}
            Parse::Bad | Parse::Done => self.pending.clear(),
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
            'h' | 'l' | 'w' | 'b' | 'e' | 'W' | 'B' | 'E' | '0' | '^' | '$' | 'j' | 'k' | 'G' | 'f' | 'F' | 't' | 'T' | ';' => {
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
                    'd' => *act = Action::ToggleDetails,
                    '?' => *act = Action::Help,
                    'x' => *act = Action::Open(Some(OpenTarget::System)),
                    'y' => *act = Action::YankPath,
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
            ';' => {
                let Some((k, ch)) = self.search_char else { return Err(Parse::Bad) };
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
                    'n' => name_object(&v, c == 'a'),
                    _ => return Err(Parse::Bad),
                };
                Motion::Range { a, b: e }
            }
            _ => return Err(Parse::Bad),
        };
        Ok((m, 1))
    }
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
}
