//! An editable directory listing. Each line carries the hidden id of the
//! entry it came from (like oil.nvim's `/001 ` prefix): editing the text
//! renames, deleting the line deletes, pasting a yanked line copies/moves,
//! and lines without an id are new files.

use crate::entry::{Branch, Entry, Git, Sort, Store};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub id: Option<u64>,
    pub text: String,
}

impl Line {
    pub fn new(text: impl Into<String>) -> Line {
        Line { id: None, text: text.into() }
    }
    pub fn chars(&self) -> usize {
        self.text.chars().count()
    }
    /// The entry name on the line: no trailing slash, no `-> target` (for
    /// symlinks and new lines; see `plan::line_parts`).
    pub fn name(&self, store: &Store) -> &str {
        crate::plan::line_parts(&self.text, self.id.and_then(|id| store.get(id))).0.trim().trim_end_matches('/')
    }
    /// The line counts for planning: blank new lines are ignored.
    pub fn significant(&self) -> bool {
        self.id.is_some() || !self.text.trim().is_empty()
    }
}

#[derive(Clone)]
struct Snap {
    lines: Vec<Line>,
    row: usize,
    col: usize,
}

pub struct Buffer {
    pub dir: PathBuf,
    pub lines: Vec<Line>,
    pristine: Vec<Line>,
    /// Ids that were listed when the buffer was (re)loaded.
    pub original: Vec<u64>,
    pub row: usize,
    pub col: usize,
    pub scroll: usize,
    pub hscroll: usize,
    pub view_h: usize,
    pub git: HashMap<String, Git>,
    pub branch: Option<Branch>,
    /// `gG`: list only entries with a git status (and folders holding some).
    pub only_changed: bool,
    /// `zm` under a content filter: list only these names.
    pub keep: Option<std::collections::HashSet<String>>,
    /// A results buffer (`alt-e` in the finder): these files, anywhere
    /// under `dir`, each listed as its path relative to `dir`.
    pub paths: Option<Vec<PathBuf>>,
    pub error: Option<String>,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    /// Bumped on every snapshot: lets `.` tell which keys changed the text.
    pub edits: u64,
}

/// Byte offset of char index `ci` in `s` (clamped to the end).
pub fn byte_at(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
}

impl Buffer {
    pub fn load(dir: &Path, store: &mut Store, hidden: bool, sort: Sort, git: bool) -> Buffer {
        let mut b = Buffer {
            dir: dir.to_path_buf(),
            lines: vec![],
            pristine: vec![],
            original: vec![],
            row: 0,
            col: 0,
            scroll: 0,
            hscroll: 0,
            view_h: 20,
            git: HashMap::new(),
            branch: None,
            only_changed: false,
            keep: None,
            paths: None,
            error: None,
            undo: vec![],
            redo: vec![],
            edits: 0,
        };
        b.reload(store, hidden, sort, git);
        b
    }

    /// A results buffer listing `paths` (files under `dir`, sorted).
    pub fn results(dir: &Path, mut paths: Vec<PathBuf>, store: &mut Store) -> Buffer {
        paths.sort_by(|a, b| crate::entry::natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
        paths.dedup();
        let mut b = Buffer::load(dir, store, false, Sort::Name, false);
        b.paths = Some(paths);
        b.reload(store, false, Sort::Name, false);
        b
    }

    /// The line text for an entry: its name here, or for a results buffer
    /// its path relative to `dir`.
    fn text_of(&self, e: &Entry) -> String {
        match e.path.parent().filter(|_| self.paths.is_some()).and_then(|p| p.strip_prefix(&self.dir).ok()) {
            Some(rel) if !rel.as_os_str().is_empty() => format!("{}/{}", rel.display(), e.display()),
            _ => e.display(),
        }
    }

    /// Re-read the directory (a results buffer: its files), discarding
    /// edits. Keeps the cursor on the same name when it still exists.
    pub fn reload(&mut self, store: &mut Store, hidden: bool, sort: Sort, git: bool) {
        let keep = self.lines.get(self.row).map(|l| l.text.clone());
        let listed = match &self.paths {
            Some(ps) => Ok(ps.iter().filter_map(|p| store.add(p)).collect()),
            None => store.list(&self.dir, hidden, sort),
        };
        match listed {
            Ok(ids) => {
                self.error = None;
                self.lines = ids.iter().map(|&id| Line { id: Some(id), text: store.get(id).map(|e| self.text_of(e)).unwrap_or_default() }).collect();
                self.original = ids;
            }
            Err(e) => {
                self.error = Some(e.to_string());
                self.lines.clear();
                self.original.clear();
            }
        }
        if self.lines.is_empty() {
            self.lines.push(Line::new(""));
        }
        // the git filter needs the status now; otherwise it may arrive later
        // from the background worker (see `App::request_git`)
        if git || self.only_changed {
            (self.git, self.branch) = crate::entry::git_status_full(&self.dir);
        }
        if self.only_changed || self.keep.is_some() {
            let (git, names, only) = (&self.git, &self.keep, self.only_changed);
            let keep = |l: &Line| {
                let name = crate::plan::split_link(&l.text).0.trim_end_matches('/');
                (!only || git.get(name).is_some_and(|g| g.letter != '!')) && names.as_ref().map_or(true, |k| k.contains(name))
            };
            self.lines.retain(keep);
            let ids: std::collections::HashSet<u64> = self.lines.iter().filter_map(|l| l.id).collect();
            self.original.retain(|id| ids.contains(id));
            if self.lines.is_empty() {
                self.lines.push(Line::new(""));
            }
        }
        self.pristine = self.lines.clone();
        self.undo.clear();
        self.redo.clear();
        if let Some(k) = keep {
            if let Some(i) = self.lines.iter().position(|l| l.text == k) {
                self.row = i;
            }
        }
        self.clamp();
    }

    /// Re-sort lines in place without touching edits (only when unmodified
    /// would be simpler, but keeping edits is friendlier).
    pub fn resort(&mut self, store: &Store, sort: Sort) {
        if self.modified() || self.paths.is_some() {
            return;
        }
        let mut es: Vec<Entry> = self.original.iter().filter_map(|id| store.get(*id).cloned()).collect();
        if es.is_empty() {
            return;
        }
        crate::entry::sort_entries(&mut es, sort, store.reverse);
        self.original = es.iter().map(|e| e.id).collect();
        self.lines = es.iter().map(|e| Line { id: Some(e.id), text: e.display() }).collect();
        if self.lines.is_empty() {
            self.lines.push(Line::new(""));
        }
        self.pristine = self.lines.clone();
        self.clamp();
    }

    pub fn modified(&self) -> bool {
        let a = self.lines.iter().filter(|l| l.significant());
        let b = self.pristine.iter().filter(|l| l.significant());
        !a.eq(b)
    }

    pub fn cur(&self) -> &Line {
        &self.lines[self.row.min(self.lines.len() - 1)]
    }

    pub fn cur_mut(&mut self) -> &mut Line {
        let r = self.row.min(self.lines.len() - 1);
        &mut self.lines[r]
    }

    pub fn snapshot(&mut self) {
        self.edits += 1;
        self.undo.push(Snap { lines: self.lines.clone(), row: self.row, col: self.col });
        if self.undo.len() > 500 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Drop the last snapshot if nothing changed since (e.g. `i` then Esc).
    /// True when it was dropped.
    pub fn squash_noop(&mut self) -> bool {
        if self.undo.last().is_some_and(|s| s.lines == self.lines) {
            self.undo.pop();
            return true;
        }
        false
    }

    /// Depth of the undo stack, for [`Buffer::squash_to`].
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Fold every snapshot taken since the stack was `len` deep into one
    /// undo step (a recipe of several commands undoes at once).
    pub fn squash_to(&mut self, len: usize) {
        self.undo.truncate(len + 1);
    }

    /// The line names an entry with a git status (ignored entries do not count).
    pub fn git_dirty(&self, row: usize) -> bool {
        let Some(l) = self.lines.get(row) else { return false };
        let name = crate::plan::split_link(&l.text).0.trim().trim_end_matches('/');
        self.git.get(name).is_some_and(|g| g.letter != '!')
    }

    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else { return false };
        self.redo.push(Snap { lines: std::mem::replace(&mut self.lines, s.lines), row: self.row, col: self.col });
        self.row = s.row;
        self.col = s.col;
        self.clamp();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else { return false };
        self.undo.push(Snap { lines: std::mem::replace(&mut self.lines, s.lines), row: self.row, col: self.col });
        self.row = s.row;
        self.col = s.col;
        self.clamp();
        true
    }

    /// Keep row/col inside the text (normal-mode rules: col < len).
    pub fn clamp(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(Line::new(""));
        }
        self.row = self.row.min(self.lines.len() - 1);
        let n = self.cur().chars();
        self.col = self.col.min(n.saturating_sub(1));
    }

    pub fn clamp_insert(&mut self) {
        self.row = self.row.min(self.lines.len() - 1);
        let n = self.cur().chars();
        self.col = self.col.min(n);
    }

    pub fn ensure_nonempty(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(Line::new(""));
        }
    }

    /// Scroll so the cursor row is visible.
    pub fn follow(&mut self) {
        let h = self.view_h.max(1);
        if self.row < self.scroll {
            self.scroll = self.row;
        } else if self.row >= self.scroll + h {
            self.scroll = self.row + 1 - h;
        }
        let max = self.lines.len().saturating_sub(h);
        self.scroll = self.scroll.min(max.max(0)).min(self.row);
    }

    /// Entry the current line came from, even while it is being renamed
    /// (for the preview and `gy`; opening still wants [`Buffer::cur_entry`]).
    pub fn cur_source_entry<'a>(&self, store: &'a Store) -> Option<&'a Entry> {
        store.get(self.cur().id?)
    }

    /// Entry of the current line if it is an unmodified, existing entry.
    pub fn cur_entry<'a>(&self, store: &'a Store) -> Option<&'a Entry> {
        let l = self.cur();
        let e = store.get(l.id?)?;
        (crate::plan::normalize(&self.dir.join(l.name(store))) == e.path).then_some(e)
    }

    // ── text primitives (char indices) ──────────────────────────────────────

    pub fn insert_str(&mut self, s: &str) {
        let col = self.col;
        let line = self.cur_mut();
        let b = byte_at(&line.text, col);
        line.text.insert_str(b, s);
        self.col += s.chars().count();
    }

    /// Delete chars [a, b) of the current line, return them.
    pub fn delete_range(&mut self, a: usize, b: usize) -> String {
        let line = self.cur_mut();
        let (ba, bb) = (byte_at(&line.text, a), byte_at(&line.text, b));
        let out = line.text[ba..bb].to_string();
        line.text.replace_range(ba..bb, "");
        out
    }

    /// Split the current line at the cursor (Enter in insert mode). The
    /// tail becomes a new line without id.
    pub fn split_line(&mut self) {
        let col = self.col;
        let line = self.cur_mut();
        let b = byte_at(&line.text, col);
        let tail = line.text.split_off(b);
        self.lines.insert(self.row + 1, Line::new(tail));
        self.row += 1;
        self.col = 0;
    }

    /// Join the current line onto the previous one (Backspace at col 0).
    pub fn join_prev(&mut self) {
        if self.row == 0 {
            return;
        }
        let l = self.lines.remove(self.row);
        self.row -= 1;
        self.col = self.cur().chars();
        self.cur_mut().text.push_str(&l.text);
    }

    pub fn join_next(&mut self) {
        if self.row + 1 >= self.lines.len() {
            return;
        }
        let l = self.lines.remove(self.row + 1);
        self.cur_mut().text.push_str(&l.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(lines: &[(Option<u64>, &str)]) -> Buffer {
        let ls: Vec<Line> = lines.iter().map(|(id, t)| Line { id: *id, text: t.to_string() }).collect();
        Buffer {
            dir: PathBuf::from("/x"),
            pristine: ls.clone(),
            lines: ls,
            original: vec![],
            row: 0,
            col: 0,
            scroll: 0,
            hscroll: 0,
            view_h: 10,
            git: HashMap::new(),
            branch: None,
            only_changed: false,
            keep: None,
            paths: None,
            error: None,
            undo: vec![],
            redo: vec![],
            edits: 0,
        }
    }

    #[test]
    fn split_and_join() {
        let mut b = buf(&[(Some(1), "hello.txt")]);
        b.col = 5;
        b.snapshot();
        b.split_line();
        assert_eq!(b.lines[0].text, "hello");
        assert_eq!(b.lines[1], Line::new(".txt"));
        assert!(b.modified());
        b.join_prev();
        assert_eq!(b.lines.len(), 1);
        assert_eq!(b.lines[0].text, "hello.txt");
        assert!(!b.modified());
    }

    #[test]
    fn undo_redo() {
        let mut b = buf(&[(Some(1), "a"), (Some(2), "b")]);
        b.snapshot();
        b.lines.remove(0);
        assert!(b.undo());
        assert_eq!(b.lines.len(), 2);
        assert!(b.redo());
        assert_eq!(b.lines.len(), 1);
    }

    #[test]
    fn blank_lines_do_not_modify() {
        let mut b = buf(&[(Some(1), "a")]);
        b.lines.push(Line::new("   "));
        assert!(!b.modified());
    }

    #[test]
    fn unicode_ranges() {
        let mut b = buf(&[(None, "äöü.md")]);
        let cut = b.delete_range(1, 3);
        assert_eq!(cut, "öü");
        assert_eq!(b.lines[0].text, "ä.md");
    }
}
