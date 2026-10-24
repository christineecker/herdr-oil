//! Structured `git diff`: plain unified output parsed into files, hunks and
//! numbered lines, word-level change ranges inside paired lines, and the
//! row model the diff viewer and the compact preview draw from.

use crate::git;
use crate::theme::Theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashMap;
use std::path::Path;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Ctx,
    Add,
    Del,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DLine {
    pub kind: Kind,
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
    pub text: String,
    /// `\ No newline at end of file` followed this line.
    pub no_eol: bool,
    /// Char ranges that differ from the paired line (word highlights).
    pub words: Vec<(usize, usize)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    /// Text after `@@ … @@` (usually the enclosing function).
    pub header: String,
    pub lines: Vec<DLine>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffFile {
    pub path: String,
    pub old_path: Option<String>,
    /// M A D R C
    pub status: char,
    pub binary: bool,
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub hunks: Vec<Hunk>,
}

impl DiffFile {
    /// (added, removed) lines.
    pub fn stat(&self) -> (usize, usize) {
        let mut a = (0, 0);
        for l in self.hunks.iter().flat_map(|h| &h.lines) {
            match l.kind {
                Kind::Add => a.0 += 1,
                Kind::Del => a.1 += 1,
                Kind::Ctx => {}
            }
        }
        a
    }
}

/// Which two states are compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cmp {
    /// HEAD → work tree (everything not committed).
    WorkTree,
    /// HEAD → index.
    Staged,
    /// index → work tree.
    Unstaged,
}

impl Cmp {
    pub fn next(self) -> Cmp {
        match self {
            Cmp::WorkTree => Cmp::Staged,
            Cmp::Staged => Cmp::Unstaged,
            Cmp::Unstaged => Cmp::WorkTree,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Cmp::WorkTree => "working tree",
            Cmp::Staged => "staged",
            Cmp::Unstaged => "unstaged",
        }
    }
    /// Column titles for the split view.
    pub fn sides(self) -> (&'static str, &'static str) {
        match self {
            Cmp::WorkTree => ("HEAD", "working tree"),
            Cmp::Staged => ("HEAD", "index"),
            Cmp::Unstaged => ("index", "working tree"),
        }
    }
}

/// Drop control characters (the text no longer comes through the ANSI parser).
fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_control() && c != '\t' { '\u{fffd}' } else { c }).collect()
}

/// `a/x b/x` → `x` when both sides name the same path.
fn same_path(rest: &str) -> Option<String> {
    let rest = rest.strip_prefix("a/")?;
    let n = rest.len().checked_sub(3)? / 2;
    let (a, b) = (rest.get(..n)?, rest.get(n..)?);
    (b.strip_prefix(" b/") == Some(a)).then(|| a.to_string())
}

fn hunk_header(l: &str) -> Option<Hunk> {
    let rest = l.strip_prefix("@@ -")?;
    let (ranges, header) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |s: &str| -> Option<(usize, usize)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (old_start, old_len) = range(old)?;
    let (new_start, new_len) = range(new)?;
    Some(Hunk { old_start, old_len, new_start, new_len, header: clean(header.trim()), lines: vec![] })
}

/// Parse `git diff` (no colour) output.
pub fn parse(text: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(l) = lines.next() {
        if let Some(rest) = l.strip_prefix("diff --git ") {
            files.push(DiffFile { path: same_path(rest).unwrap_or_default(), status: 'M', ..Default::default() });
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if let Some(h) = hunk_header(l) {
            let mut h = h;
            let (mut o, mut n) = (h.old_start, h.new_start);
            let (mut old_left, mut new_left) = (h.old_len, h.new_len);
            while old_left > 0 || new_left > 0 {
                let Some(&next) = lines.peek() else { break };
                let (kind, body) = match next.chars().next() {
                    Some(' ') => (Kind::Ctx, &next[1..]),
                    Some('+') => (Kind::Add, &next[1..]),
                    Some('-') => (Kind::Del, &next[1..]),
                    Some('\\') => {
                        if let Some(last) = h.lines.last_mut() {
                            last.no_eol = true;
                        }
                        lines.next();
                        continue;
                    }
                    // an empty context line can lose its leading space
                    None => (Kind::Ctx, ""),
                    _ => break,
                };
                lines.next();
                let (old_no, new_no) = match kind {
                    Kind::Ctx => {
                        old_left = old_left.saturating_sub(1);
                        new_left = new_left.saturating_sub(1);
                        (Some(o), Some(n))
                    }
                    Kind::Add => {
                        new_left = new_left.saturating_sub(1);
                        (None, Some(n))
                    }
                    Kind::Del => {
                        old_left = old_left.saturating_sub(1);
                        (Some(o), None)
                    }
                };
                if old_no.is_some() {
                    o += 1;
                }
                if new_no.is_some() {
                    n += 1;
                }
                h.lines.push(DLine { kind, old_no, new_no, text: clean(body), no_eol: false, words: vec![] });
            }
            if lines.peek().is_some_and(|l| l.starts_with('\\')) {
                if let Some(last) = h.lines.last_mut() {
                    last.no_eol = true;
                }
                lines.next();
            }
            f.hunks.push(h);
            continue;
        }
        if let Some(p) = l.strip_prefix("rename from ").or_else(|| l.strip_prefix("copy from ")) {
            f.old_path = Some(clean(p));
            f.status = if l.starts_with("rename") { 'R' } else { 'C' };
        } else if let Some(p) = l.strip_prefix("rename to ").or_else(|| l.strip_prefix("copy to ")) {
            f.path = clean(p);
        } else if l.starts_with("new file mode") {
            f.status = 'A';
            f.new_mode = l.rsplit(' ').next().map(str::to_string);
        } else if l.starts_with("deleted file mode") {
            f.status = 'D';
            f.old_mode = l.rsplit(' ').next().map(str::to_string);
        } else if let Some(m) = l.strip_prefix("old mode ") {
            f.old_mode = Some(m.to_string());
        } else if let Some(m) = l.strip_prefix("new mode ") {
            f.new_mode = Some(m.to_string());
        } else if l.starts_with("Binary files ") || l == "GIT binary patch" {
            f.binary = true;
        } else if let Some(p) = l.strip_prefix("+++ ") {
            if let Some(p) = p.strip_prefix("b/") {
                f.path = clean(p);
            }
        } else if let Some(p) = l.strip_prefix("--- ") {
            if f.path.is_empty() {
                if let Some(p) = p.strip_prefix("a/") {
                    f.path = clean(p);
                }
            }
        }
    }
    for f in &mut files {
        annotate_words(f);
    }
    files
}

// ── word-level highlights ──────────────────────────────────────────────────

/// Tokens as char ranges: whitespace runs, quoted strings, identifiers /
/// numbers, single punctuation.
pub fn tokens(s: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        let start = i;
        if c.is_whitespace() {
            while i < s.len() && s[i].is_whitespace() {
                i += 1;
            }
        } else if c == '"' || c == '\'' || c == '`' {
            i += 1;
            while i < s.len() && s[i] != c {
                i += if s[i] == '\\' { 2 } else { 1 };
            }
            i = (i + 1).min(s.len());
        } else if c.is_alphanumeric() || c == '_' {
            while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_') {
                i += 1;
            }
        } else {
            i += 1;
        }
        out.push((start, i));
    }
    out
}

/// Char ranges `[a, b)` within a line.
pub type Ranges = Vec<(usize, usize)>;

/// Changed char ranges in `a` and `b`: tokens outside their longest common
/// subsequence. Whitespace only counts when both neighbours changed.
pub fn word_ranges(a: &str, b: &str) -> (Ranges, Ranges) {
    let (ac, bc): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (ta, tb) = (tokens(&ac), tokens(&bc));
    let eq = |x: (usize, usize), y: (usize, usize)| ac[x.0..x.1] == bc[y.0..y.1];
    let (n, m) = (ta.len(), tb.len());
    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if eq(ta[i], tb[j]) { dp[i + 1][j + 1] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
        }
    }
    let (mut ka, mut kb) = (vec![false; n], vec![false; m]);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if eq(ta[i], tb[j]) {
            ka[i] = true;
            kb[j] = true;
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let ranges = |toks: &[(usize, usize)], keep: &[bool], chars: &[char]| {
        let ws = |k: usize| chars[toks[k].0].is_whitespace();
        let solid: Vec<bool> = (0..toks.len()).map(|k| !ws(k) && !keep[k]).collect();
        // whitespace joins two changed words, and is never marked on its own
        let changed: Vec<bool> = (0..toks.len()).map(|k| if ws(k) { k > 0 && k + 1 < toks.len() && solid[k - 1] && solid[k + 1] } else { solid[k] }).collect();
        let mut out: Vec<(usize, usize)> = Vec::new();
        for (k, t) in toks.iter().enumerate() {
            if !changed[k] {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.1 == t.0 => last.1 = t.1,
                _ => out.push(*t),
            }
        }
        out
    };
    (ranges(&ta, &ka, &ac), ranges(&tb, &kb, &bc))
}

const WORD_MAX_LINE: usize = 500;
const WORD_MAX_BLOCK: usize = 200;

/// Pair the i-th removed with the i-th added line of every change block and
/// store their word ranges (skipped for very long lines / blocks).
fn annotate_words(f: &mut DiffFile) {
    for h in &mut f.hunks {
        let mut i = 0;
        while i < h.lines.len() {
            if h.lines[i].kind != Kind::Del {
                i += 1;
                continue;
            }
            let d0 = i;
            while i < h.lines.len() && h.lines[i].kind == Kind::Del {
                i += 1;
            }
            let a0 = i;
            while i < h.lines.len() && h.lines[i].kind == Kind::Add {
                i += 1;
            }
            let (dels, adds) = (a0 - d0, i - a0);
            if dels + adds > WORD_MAX_BLOCK {
                continue;
            }
            for k in 0..dels.min(adds) {
                let (x, y) = (d0 + k, a0 + k);
                if h.lines[x].text.len() > WORD_MAX_LINE || h.lines[y].text.len() > WORD_MAX_LINE {
                    continue;
                }
                let (wa, wb) = word_ranges(&h.lines[x].text, &h.lines[y].text);
                // a line rewritten from scratch reads better without word marks
                let total = |w: &[(usize, usize)]| w.iter().map(|(a, b)| b - a).sum::<usize>();
                if total(&wa) * 10 > h.lines[x].text.chars().count() * 9 && total(&wb) * 10 > h.lines[y].text.chars().count() * 9 {
                    continue;
                }
                h.lines[x].words = wa;
                h.lines[y].words = wb;
            }
        }
    }
}

// ── running git ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Opts {
    pub cmp: Cmp,
    pub ignore_ws: bool,
    pub context: usize,
}

impl Default for Opts {
    fn default() -> Self {
        Opts { cmp: Cmp::WorkTree, ignore_ws: false, context: 3 }
    }
}

/// Cap on diff output read for one file.
const MAX_BYTES: usize = 8 << 20;

/// Diff of one path (`untracked`: show it as all added).
pub fn load(path: &Path, untracked: bool, o: Opts) -> Result<Vec<DiffFile>, String> {
    let root = git::guard(path)?;
    let rel = git::rel(&root, path).ok_or("outside the repository")?;
    let mut c = git::cmd(&root);
    c.args(["diff", "--no-color", "--no-ext-diff", "--no-textconv", "-M"]).arg(format!("-U{}", o.context));
    if o.ignore_ws {
        c.arg("-w");
    }
    if untracked {
        if o.cmp == Cmp::Staged {
            return Ok(vec![]);
        }
        c.args(["--no-index", "--", "/dev/null", &rel]);
    } else {
        match o.cmp {
            Cmp::WorkTree => {
                c.arg(git::base(&root));
            }
            Cmp::Staged => {
                c.arg("--cached").arg(git::base(&root));
            }
            Cmp::Unstaged => {}
        }
        c.args(["--", &rel]);
    }
    let out = c.stderr(std::process::Stdio::piped()).output().map_err(|e| format!("git: {e}"))?;
    // --no-index exits 1 when the files differ
    if !out.status.success() && !(untracked && out.status.code() == Some(1)) {
        return Err(String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("git diff failed").to_string());
    }
    let bytes = &out.stdout[..out.stdout.len().min(MAX_BYTES)];
    let mut files = parse(&String::from_utf8_lossy(bytes));
    if untracked {
        for f in &mut files {
            f.path = rel.clone();
            f.status = 'A';
        }
    }
    Ok(files)
}

/// `git diff --numstat` for the whole repository: path → (added, removed).
pub fn numstat(root: &Path, cmp: Cmp) -> HashMap<String, (usize, usize)> {
    let mut c = git::cmd(root);
    c.args(["diff", "--numstat", "-z", "--no-color", "--no-ext-diff", "--no-textconv", "-M"]);
    match cmp {
        Cmp::WorkTree => {
            c.arg(git::base(root));
        }
        Cmp::Staged => {
            c.arg("--cached").arg(git::base(root));
        }
        Cmp::Unstaged => {}
    }
    let Ok(out) = c.stderr(std::process::Stdio::null()).output() else { return HashMap::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut map = HashMap::new();
    let mut it = text.split('\0');
    while let Some(rec) = it.next() {
        let mut f = rec.splitn(3, '\t');
        let (Some(a), Some(d), Some(p)) = (f.next(), f.next(), f.next()) else { continue };
        let n = |s: &str| s.parse().unwrap_or(0);
        // a rename: empty path field, then old and new names
        let path = if p.is_empty() {
            it.next();
            it.next().unwrap_or("").to_string()
        } else {
            p.to_string()
        };
        map.insert(path, (n(a), n(d)));
    }
    map
}

// ── rendering ──────────────────────────────────────────────────────────────

/// One screen row of a diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// Unchanged lines not shown before hunk `h` (`h == hunks.len()`: after the last).
    Fold { h: usize, count: usize },
    Hunk(usize),
    Line { h: usize, l: usize },
    /// Split view: a removed / context line on the left, added / context on the right.
    Pair { h: usize, left: Option<usize>, right: Option<usize> },
    /// Binary, mode-only or empty diffs.
    Note,
}

/// Rows of one file. `total_new`: line count of the new side when known
/// (for the fold after the last hunk).
pub fn rows(f: &DiffFile, split: bool, total_new: Option<usize>) -> Vec<Row> {
    let mut out = Vec::new();
    if f.hunks.is_empty() {
        out.push(Row::Note);
        return out;
    }
    let mut next_old = 1;
    for (hi, h) in f.hunks.iter().enumerate() {
        let gap = h.old_start.saturating_sub(next_old);
        if gap > 0 && h.old_len > 0 {
            out.push(Row::Fold { h: hi, count: gap });
        }
        out.push(Row::Hunk(hi));
        if split {
            let mut i = 0;
            while i < h.lines.len() {
                if h.lines[i].kind == Kind::Ctx {
                    out.push(Row::Pair { h: hi, left: Some(i), right: Some(i) });
                    i += 1;
                    continue;
                }
                let d0 = i;
                while i < h.lines.len() && h.lines[i].kind == Kind::Del {
                    i += 1;
                }
                let a0 = i;
                while i < h.lines.len() && h.lines[i].kind == Kind::Add {
                    i += 1;
                }
                let (dels, adds) = (a0 - d0, i - a0);
                for k in 0..dels.max(adds) {
                    out.push(Row::Pair { h: hi, left: (k < dels).then_some(d0 + k), right: (k < adds).then_some(a0 + k) });
                }
            }
        } else {
            out.extend((0..h.lines.len()).map(|l| Row::Line { h: hi, l }));
        }
        next_old = h.old_start + h.old_len;
    }
    if let (Some(total), Some(last)) = (total_new, f.hunks.last()) {
        let end = last.new_start + last.new_len;
        if total + 1 > end {
            out.push(Row::Fold { h: f.hunks.len(), count: total + 1 - end });
        }
    }
    out
}

/// New-side line a row stands for (for "open at line"): the line itself,
/// or the next new-side line after a removal, the hunk start for headers.
pub fn row_line(f: &DiffFile, r: Row) -> Option<usize> {
    let h = |i: usize| f.hunks.get(i);
    match r {
        Row::Hunk(i) | Row::Fold { h: i, .. } => h(i).map(|h| h.new_start.max(1)).or_else(|| f.hunks.last().map(|h| h.new_start + h.new_len)),
        Row::Line { h: hi, l } | Row::Pair { h: hi, left: Some(l), right: None } | Row::Pair { h: hi, right: Some(l), .. } => {
            let hk = h(hi)?;
            hk.lines[l..].iter().find_map(|x| x.new_no).or(Some(hk.new_start + hk.new_len))
        }
        Row::Pair { left: None, right: None, .. } | Row::Note => None,
    }
}

/// Plain text of a row, for search.
pub fn row_text(f: &DiffFile, r: Row) -> String {
    match r {
        Row::Hunk(i) => f.hunks.get(i).map(|h| h.header.clone()).unwrap_or_default(),
        Row::Line { h, l } => f.hunks[h].lines[l].text.clone(),
        Row::Pair { h, left, right } => {
            let t = |i: Option<usize>| i.map(|i| f.hunks[h].lines[i].text.clone()).unwrap_or_default();
            format!("{} {}", t(left), t(right))
        }
        Row::Fold { .. } | Row::Note => String::new(),
    }
}

/// Colours of a diff, derived from the theme.
pub struct Palette {
    pub add_line: Option<Color>,
    pub add_gutter: Option<Color>,
    pub add_word: Option<Color>,
    pub del_line: Option<Color>,
    pub del_gutter: Option<Color>,
    pub del_word: Option<Color>,
    pub hunk: Option<Color>,
    pub filler: Option<Color>,
}

impl Palette {
    pub fn new(th: &Theme) -> Palette {
        Palette {
            add_line: th.tint(th.green, 0.14),
            add_gutter: th.tint(th.green, 0.20),
            add_word: th.tint(th.green, 0.30),
            del_line: th.tint(th.red, 0.14),
            del_gutter: th.tint(th.red, 0.20),
            del_word: th.tint(th.red, 0.30),
            hunk: th.tint(th.blue, 0.10),
            filler: th.truecolor.then_some(Color::Rgb(0x26, 0x26, 0x29)),
        }
    }
}

fn bg(s: Style, c: Option<Color>) -> Style {
    match c {
        Some(c) => s.bg(c),
        None => s,
    }
}

/// Per-char foreground colours of the old / new file (bat), by line number.
#[derive(Default)]
pub struct Syntax {
    pub old: Vec<Vec<Option<Color>>>,
    pub new: Vec<Vec<Option<Color>>>,
}

/// Cells of a code line: tabs expanded, word ranges and syntax colours
/// applied, clipped to `width` after skipping `skip` columns. Returns spans.
#[allow(clippy::too_many_arguments)]
pub fn code_spans(text: &str, words: &[(usize, usize)], base: Style, word: Style, syntax: Option<&Vec<Option<Color>>>, skip: usize, width: usize, fill: bool) -> Vec<Span<'static>> {
    let mut cells: Vec<(char, Style)> = Vec::new();
    let mut col = 0;
    for (i, c) in text.chars().enumerate() {
        let mut st = if words.iter().any(|&(a, b)| i >= a && i < b) { word } else { base };
        if let Some(fg) = syntax.and_then(|s| s.get(i).copied().flatten()) {
            st = st.fg(fg);
        }
        if c == '\t' {
            let n = 4 - col % 4;
            for _ in 0..n {
                cells.push((' ', st));
            }
            col += n;
        } else {
            let w = c.width().unwrap_or(0);
            if w == 0 {
                continue;
            }
            cells.push((c, st));
            col += w;
        }
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for (c, st) in cells.into_iter().skip(skip) {
        let w = c.width().unwrap_or(1);
        if used + w > width {
            break;
        }
        used += w;
        match out.last_mut() {
            Some(last) if last.style == st => last.content.to_mut().push(c),
            _ => out.push(Span::styled(c.to_string(), st)),
        }
    }
    if fill && used < width {
        out.push(Span::styled(" ".repeat(width - used), base));
    }
    out
}

/// Five-block stat bar: green blocks for the share of additions.
pub fn stat_bar(th: &Theme, add: usize, del: usize) -> Vec<Span<'static>> {
    let total = add + del;
    let g = if total == 0 { 0 } else { ((5 * add) as f32 / total as f32).round() as usize };
    let r = if total == 0 { 0 } else { 5 - g };
    vec![
        Span::styled("■".repeat(g), Style::default().fg(th.green)),
        Span::styled("■".repeat(r), Style::default().fg(th.red)),
        Span::styled("■".repeat(5 - g - r), Style::default().fg(th.dim)),
    ]
}

/// File header: status letter, `dir/` + **name**, `+N −M`, stat bar.
pub fn file_header(th: &Theme, f: &DiffFile) -> Line<'static> {
    let (a, d) = f.stat();
    let col = match f.status {
        'A' => th.green,
        'D' => th.red,
        'R' | 'C' => th.blue,
        _ => th.yellow,
    };
    let (dir, name) = match f.path.rfind('/') {
        Some(i) => (f.path[..=i].to_string(), f.path[i + 1..].to_string()),
        None => (String::new(), f.path.clone()),
    };
    let mut v = vec![
        Span::styled(format!(" {} ", f.status), Style::default().fg(col).add_modifier(Modifier::BOLD)),
        Span::styled(dir, Style::default().fg(th.subtext)),
        Span::styled(name, Style::default().fg(th.text).add_modifier(Modifier::BOLD)),
    ];
    if let Some(o) = &f.old_path {
        v.push(Span::styled(format!("  ← {o}"), Style::default().fg(th.dim)));
    }
    v.push(Span::styled(format!("  +{a}"), Style::default().fg(th.green)));
    v.push(Span::styled(format!(" −{d}  "), Style::default().fg(th.red)));
    v.extend(stat_bar(th, a, d));
    Line::from(v)
}

/// What a file without hunks changed.
pub fn note(f: &DiffFile) -> String {
    if f.binary {
        return "binary file changed".into();
    }
    match (&f.old_mode, &f.new_mode) {
        (Some(a), Some(b)) => format!("mode {a} → {b}"),
        _ if f.status == 'R' => "renamed, content unchanged".into(),
        _ if f.status == 'A' => "new empty file".into(),
        _ if f.status == 'D' => "deleted empty file".into(),
        _ => "no changes".into(),
    }
}

fn digits(n: usize) -> usize {
    n.max(1).to_string().len()
}

/// Width of the line-number columns for a file.
pub fn num_width(f: &DiffFile) -> usize {
    let max = f.hunks.iter().map(|h| (h.old_start + h.old_len).max(h.new_start + h.new_len)).max().unwrap_or(1);
    digits(max).max(3)
}

/// One unified row.
#[allow(clippy::too_many_arguments)]
pub fn unified_row(th: &Theme, p: &Palette, f: &DiffFile, r: Row, width: usize, skip: usize, syn: Option<&Syntax>, nw: usize) -> Line<'static> {
    let dim = Style::default().fg(th.dim);
    match r {
        Row::Note => Line::styled(format!("  {}", note(f)), dim.add_modifier(Modifier::ITALIC)),
        Row::Fold { count, .. } => Line::from(vec![
            Span::styled(format!("{:>w$}  ", "↕", w = nw * 2 + 1), Style::default().fg(th.accent)),
            Span::styled(format!("Expand {count} unchanged line{}", if count == 1 { "" } else { "s" }), dim),
            Span::styled("   (z)", dim),
        ]),
        Row::Hunk(i) => {
            let h = &f.hunks[i];
            let text = format!("{:>w$}  @@ -{},{} +{},{} @@ {}", "⋯", h.old_start, h.old_len, h.new_start, h.new_len, h.header, w = nw * 2 + 1);
            let st = bg(Style::default().fg(th.blue), p.hunk);
            let pad = width.saturating_sub(unicode_width::UnicodeWidthStr::width(text.as_str()));
            Line::from(vec![Span::styled(text, st), Span::styled(" ".repeat(pad), st)])
        }
        Row::Line { h, l } => {
            let x = &f.hunks[h].lines[l];
            let num = |n: Option<usize>| n.map(|n| format!("{n:>nw$}")).unwrap_or_else(|| " ".repeat(nw));
            let (sign, line_bg, gut_bg, word_bg, sign_col) = match x.kind {
                Kind::Add => ("+", p.add_line, p.add_gutter, p.add_word, th.green),
                Kind::Del => ("-", p.del_line, p.del_gutter, p.del_word, th.red),
                Kind::Ctx => (" ", None, None, None, th.dim),
            };
            let num_fg = if x.kind == Kind::Ctx { th.dim } else { sign_col };
            let gut = bg(Style::default().fg(num_fg), gut_bg);
            let mut v = vec![
                Span::styled(format!("{} {} ", num(x.old_no), num(x.new_no)), gut),
                Span::styled(format!("{sign} "), bg(Style::default().fg(sign_col).add_modifier(Modifier::BOLD), line_bg)),
            ];
            let used = nw * 2 + 4;
            let base = bg(Style::default().fg(th.text), line_bg);
            let word = match word_bg {
                Some(c) => base.bg(c),
                None => base.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            };
            let sy = syn.and_then(|s| match x.kind {
                Kind::Del => x.old_no.and_then(|n| s.old.get(n - 1)),
                _ => x.new_no.and_then(|n| s.new.get(n - 1)),
            });
            v.extend(code_spans(&x.text, &x.words, base, word, sy, skip, width.saturating_sub(used), line_bg.is_some()));
            if x.no_eol {
                v.push(Span::styled(" ⏎̸", dim));
            }
            Line::from(v)
        }
        Row::Pair { .. } => Line::raw(""),
    }
}

/// One split row: two halves of `half` columns each, joined by a rule.
#[allow(clippy::too_many_arguments)]
pub fn split_row(th: &Theme, p: &Palette, f: &DiffFile, r: Row, width: usize, skip: usize, syn: Option<&Syntax>, nw: usize) -> Line<'static> {
    let Row::Pair { h, left, right } = r else {
        return unified_row(th, p, f, r, width, skip, syn, nw);
    };
    let half = width.saturating_sub(1) / 2;
    let side = |i: Option<usize>, left_side: bool| -> Vec<Span<'static>> {
        let Some(i) = i else {
            return vec![Span::styled(" ".repeat(half), bg(Style::default(), p.filler))];
        };
        let x = &f.hunks[h].lines[i];
        let (no, sign, line_bg, gut_bg, word_bg, col) = match (x.kind, left_side) {
            (Kind::Del, _) => (x.old_no, "-", p.del_line, p.del_gutter, p.del_word, th.red),
            (Kind::Add, _) => (x.new_no, "+", p.add_line, p.add_gutter, p.add_word, th.green),
            (Kind::Ctx, true) => (x.old_no, " ", None, None, None, th.dim),
            (Kind::Ctx, false) => (x.new_no, " ", None, None, None, th.dim),
        };
        let mut v = vec![
            Span::styled(format!("{:>nw$} ", no.map(|n| n.to_string()).unwrap_or_default()), bg(Style::default().fg(col), gut_bg)),
            Span::styled(format!("{sign} "), bg(Style::default().fg(col).add_modifier(Modifier::BOLD), line_bg)),
        ];
        let base = bg(Style::default().fg(th.text), line_bg);
        let word = match word_bg {
            Some(c) => base.bg(c),
            None => base.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        };
        let sy = syn.and_then(|s| if x.kind == Kind::Del || (x.kind == Kind::Ctx && left_side) { x.old_no.and_then(|n| s.old.get(n - 1)) } else { x.new_no.and_then(|n| s.new.get(n - 1)) });
        v.extend(code_spans(&x.text, &x.words, base, word, sy, skip, half.saturating_sub(nw + 3), true));
        v
    };
    let mut v = side(left, true);
    v.push(Span::styled("│", Style::default().fg(th.dim)));
    v.extend(side(right, false));
    Line::from(v)
}

/// The compact unified rendering for side previews: file headers, hunks
/// and lines, no folds.
pub fn render_compact(files: &[DiffFile], th: &Theme, width: u16, max: usize) -> Vec<Line<'static>> {
    let p = Palette::new(th);
    let mut out = Vec::new();
    for f in files {
        out.push(file_header(th, f));
        let nw = num_width(f);
        for r in rows(f, false, None) {
            if matches!(r, Row::Fold { .. }) {
                continue;
            }
            out.push(unified_row(th, &p, f, r, width as usize, 0, None, nw));
            if out.len() >= max {
                return out;
            }
        }
        out.push(Line::raw(""));
    }
    out
}

/// bat colours for a whole file's text (None when bat is missing or the
/// file is too large to bother).
pub fn syntax_of(text: &str, name: &str) -> Option<Vec<Vec<Option<Color>>>> {
    if text.len() > 2 << 20 || text.lines().count() > 20_000 {
        return None;
    }
    let bat = crate::preview::bat_bin()?;
    let mut child = std::process::Command::new(bat)
        .args(["--color=always", "--style=plain", "--paging=never", "--wrap=never", "--tabs=0"])
        .arg(format!("--file-name={name}"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let owned = text.to_string();
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        let _ = stdin.write_all(owned.as_bytes());
    });
    let out = child.wait_with_output().ok()?;
    let _ = writer.join();
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    Some(
        s.lines()
            .map(|l| {
                let line = crate::preview::parse_ansi_tabs(l, false);
                line.spans.iter().flat_map(|sp| sp.content.chars().map(move |_| sp.style.fg)).collect()
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,4 +1,5 @@ fn main() {
 one
-two
+two!
+three
 four
-five
\\ No newline at end of file
+five
diff --git a/old name.txt b/new name.txt
similarity index 90%
rename from old name.txt
rename to new name.txt
index 333..444 100644
--- a/old name.txt
+++ b/new name.txt
@@ -10 +10 @@
--- not a header
+++ also not
diff --git a/img.png b/img.png
new file mode 100644
index 0000000..555
Binary files /dev/null and b/img.png differ
diff --git a/run.sh b/run.sh
old mode 100644
new mode 100755
";

    #[test]
    fn parses_files_hunks_and_numbers() {
        let fs = parse(SAMPLE);
        assert_eq!(fs.len(), 4);
        let a = &fs[0];
        assert_eq!((a.path.as_str(), a.status), ("src/a.rs", 'M'));
        let h = &a.hunks[0];
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len, h.header.as_str()), (1, 4, 1, 5, "fn main() {"));
        let nos: Vec<_> = h.lines.iter().map(|l| (l.kind, l.old_no, l.new_no)).collect();
        assert_eq!(
            nos,
            [
                (Kind::Ctx, Some(1), Some(1)),
                (Kind::Del, Some(2), None),
                (Kind::Add, None, Some(2)),
                (Kind::Add, None, Some(3)),
                (Kind::Ctx, Some(3), Some(4)),
                (Kind::Del, Some(4), None),
                (Kind::Add, None, Some(5)),
            ]
        );
        assert!(h.lines[5].no_eol && !h.lines[6].no_eol);
        assert_eq!(a.stat(), (3, 2));
        // two! vs two: only the "!" differs
        assert_eq!(h.lines[2].words, vec![(3, 4)]);
        let r = &fs[1];
        assert_eq!((r.status, r.path.as_str(), r.old_path.as_deref()), ('R', "new name.txt", Some("old name.txt")));
        assert_eq!(r.hunks[0].lines.len(), 2, "body lines that look like headers stay lines");
        assert_eq!(r.hunks[0].lines[0].text, "-- not a header");
        assert!(fs[2].binary && fs[2].status == 'A');
        assert_eq!(note(&fs[3]), "mode 100644 → 100755");
    }

    #[test]
    fn word_ranges_skip_lone_whitespace() {
        let (a, b) = word_ranges("let x = foo(1, 2);", "let y = foo(1, 3);");
        assert_eq!(a, vec![(4, 5), (15, 16)]);
        assert_eq!(b, vec![(4, 5), (15, 16)]);
        let (a, _) = word_ranges("a b c", "x y c");
        assert_eq!(a, vec![(0, 3)], "space between two changed words joins them");
    }

    #[test]
    fn split_pairs_change_blocks() {
        let fs = parse(SAMPLE);
        let rs = rows(&fs[0], true, Some(9));
        assert_eq!(rs[0], Row::Hunk(0));
        assert_eq!(rs[2], Row::Pair { h: 0, left: Some(1), right: Some(2) });
        assert_eq!(rs[3], Row::Pair { h: 0, left: None, right: Some(3) });
        assert_eq!(*rs.last().unwrap(), Row::Fold { h: 1, count: 4 });
        assert_eq!(row_line(&fs[0], rs[3]), Some(3));
    }

    #[test]
    fn loads_untracked_and_staged() {
        let t = crate::plan::tests::tmpdir("diff-load");
        let g = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&t.0).output().unwrap();
        g(&["init", "-q"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "t"]);
        std::fs::write(t.0.join("new.txt"), "a\nb\n").unwrap();
        let fs = load(&t.0.join("new.txt"), true, Opts::default()).unwrap();
        assert_eq!((fs[0].status, fs[0].path.as_str(), fs[0].stat()), ('A', "new.txt", (2, 0)));
        // staged before the first commit diffs against the empty tree
        g(&["add", "new.txt"]);
        let fs = load(&t.0.join("new.txt"), false, Opts { cmp: Cmp::Staged, ..Opts::default() }).unwrap();
        assert_eq!(fs[0].stat(), (2, 0));
        g(&["commit", "-qm", "x"]);
        std::fs::write(t.0.join("new.txt"), "a\nB\n").unwrap();
        let fs = load(&t.0.join("new.txt"), false, Opts::default()).unwrap();
        assert_eq!(fs[0].stat(), (1, 1));
        assert!(load(&t.0.join("new.txt"), false, Opts { cmp: Cmp::Staged, ..Opts::default() }).unwrap().is_empty());
        let ns = numstat(&t.0, Cmp::WorkTree);
        assert_eq!(ns.get("new.txt"), Some(&(1, 1)));
    }
}
