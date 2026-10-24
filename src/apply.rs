//! Execute a validated plan safely.
//!
//! 1. copies are made into hidden staging names (sources still pristine),
//! 2. moves and deletes run deepest-path first; a moved entry is renamed into
//!    a staging name inside the nearest ancestor that itself never moves, so
//!    swaps (a↔b), case-only renames and "move child out, delete parent" work,
//! 3. creates and staged entries land at their final paths, shallowest
//!    first, with destinations re-rooted under directories that moved.
//! Nothing is overwritten: an occupied destination is reported and the
//! staged entry is left where it is (named in the report).

use crate::config::DeleteMethod;
use crate::plan::{key, Op};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Report {
    pub done: usize,
    pub errors: Vec<String>,
    /// Final paths of everything created/moved/copied.
    pub landed: Vec<PathBuf>,
}

fn depth(p: &Path) -> usize {
    p.components().count()
}

pub fn copy_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    let m = fs::symlink_metadata(src)?;
    let ft = m.file_type();
    if ft.is_symlink() {
        let target = fs::read_link(src)?;
        std::os::unix::fs::symlink(target, dst)
    } else if ft.is_dir() {
        fs::create_dir(dst)?;
        for de in fs::read_dir(src)? {
            let de = de?;
            copy_recursive(&de.path(), &dst.join(de.file_name()))?;
        }
        fs::set_permissions(dst, m.permissions())
    } else {
        fs::copy(src, dst).map(|_| ())
    }
}

pub fn remove_any(p: &Path) -> io::Result<()> {
    let m = fs::symlink_metadata(p)?;
    if m.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

/// rename, falling back to copy + remove across filesystems.
pub fn move_path(src: &Path, dst: &Path) -> io::Result<()> {
    match fs::rename(src, dst) {
        Err(e) if e.raw_os_error() == Some(18) => {
            copy_recursive(src, dst)?;
            remove_any(src)
        }
        r => r,
    }
}

fn unique(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 2.. {
        let p = dir.join(format!("{stem} {n}{ext}"));
        if fs::symlink_metadata(&p).is_err() {
            return p;
        }
    }
    unreachable!()
}

pub fn trash(p: &Path) -> io::Result<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| io::Error::other("HOME unset"))?);
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "item".into());
    if cfg!(target_os = "macos") {
        let dir = home.join(".Trash");
        fs::create_dir_all(&dir)?;
        let dst = unique(&dir, &name);
        move_path(p, &dst)?;
        Ok(dst)
    } else {
        let base = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".local/share"))
            .join("Trash");
        let (files, info) = (base.join("files"), base.join("info"));
        fs::create_dir_all(&files)?;
        fs::create_dir_all(&info)?;
        let dst = unique(&files, &name);
        let iname = format!("{}.trashinfo", dst.file_name().unwrap().to_string_lossy());
        let now = std::process::Command::new("date")
            .arg("+%Y-%m-%dT%H:%M:%S")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        fs::write(info.join(iname), format!("[Trash Info]\nPath={}\nDeletionDate={now}\n", p.display()))?;
        move_path(p, &dst)?;
        Ok(dst)
    }
}

struct Ctx {
    vacated: HashSet<String>,
    moves: HashMap<String, PathBuf>,
    n: usize,
}

impl Ctx {
    /// Nearest ancestor of `p` that is never moved or deleted (nor are any
    /// of its own ancestors).
    fn stable_dir(&self, p: &Path) -> PathBuf {
        let mut best = PathBuf::from("/");
        let mut cur = PathBuf::new();
        let parent = p.parent().unwrap_or(Path::new("/"));
        for c in parent.components() {
            cur.push(c.as_os_str());
            if self.vacated.contains(&key(&cur)) {
                break;
            }
            best = cur.clone();
        }
        best
    }

    fn stage_path(&mut self, src: &Path) -> PathBuf {
        let dir = self.stable_dir(src);
        loop {
            self.n += 1;
            let p = dir.join(format!(".herdr-oil~{}-{}", std::process::id(), self.n));
            if fs::symlink_metadata(&p).is_err() {
                return p;
            }
        }
    }

    /// Where an original-coordinates path ends up after all moves.
    fn final_path(&self, p: &Path, guard: usize) -> PathBuf {
        if guard > 128 {
            return p.to_path_buf();
        }
        if let Some(d) = self.moves.get(&key(p)) {
            return self.final_dst(d, guard + 1);
        }
        match (p.parent(), p.file_name()) {
            (Some(par), Some(name)) => self.final_path(par, guard + 1).join(name),
            _ => p.to_path_buf(),
        }
    }

    fn final_dst(&self, d: &Path, guard: usize) -> PathBuf {
        match (d.parent(), d.file_name()) {
            (Some(par), Some(name)) => self.final_path(par, guard).join(name),
            _ => d.to_path_buf(),
        }
    }
}

enum Landing {
    Staged(PathBuf),
    Create(bool),
}

pub fn apply(ops: &[Op], method: DeleteMethod) -> Report {
    let mut rep = Report::default();
    let mut ctx = Ctx { vacated: HashSet::new(), moves: HashMap::new(), n: 0 };
    let mut deleted = HashSet::new();
    for op in ops {
        match op {
            Op::Move { src, dst, .. } => {
                ctx.vacated.insert(key(src));
                ctx.moves.insert(key(src), dst.clone());
            }
            Op::Delete { src, .. } => {
                ctx.vacated.insert(key(src));
                deleted.insert(key(src));
            }
            _ => {}
        }
    }
    let mut landings: Vec<(PathBuf, Landing, Option<PathBuf>)> = Vec::new();

    // 1. copies
    for op in ops {
        if let Op::Copy { src, dst, .. } = op {
            let stage = ctx.stage_path(src);
            match copy_recursive(src, &stage) {
                Ok(()) => landings.push((dst.clone(), Landing::Staged(stage), None)),
                Err(e) => {
                    let _ = remove_any(&stage);
                    rep.errors.push(format!("copy {}: {e}", src.display()));
                }
            }
        }
    }

    // 2. moves + deletes, deepest first
    let mut md: Vec<&Op> = ops.iter().filter(|o| matches!(o, Op::Move { .. } | Op::Delete { .. })).collect();
    md.sort_by_key(|o| std::cmp::Reverse(depth(o.src().unwrap())));
    for op in md {
        match op {
            Op::Move { src, dst, .. } => {
                let stage = ctx.stage_path(src);
                match move_path(src, &stage) {
                    Ok(()) => landings.push((dst.clone(), Landing::Staged(stage), Some(src.clone()))),
                    Err(e) => rep.errors.push(format!("move {}: {e}", src.display())),
                }
            }
            Op::Delete { src, .. } => {
                let under_deleted = src.ancestors().skip(1).any(|a| deleted.contains(&key(a)));
                if under_deleted {
                    rep.done += 1;
                    continue;
                }
                let r = match method {
                    DeleteMethod::Trash => trash(src).map(|_| ()),
                    DeleteMethod::Permanent => remove_any(src),
                };
                match r {
                    Ok(()) => rep.done += 1,
                    Err(e) => rep.errors.push(format!("delete {}: {e}", src.display())),
                }
            }
            _ => {}
        }
    }

    // 3. land creates + staged entries, shallowest final path first
    for op in ops {
        if let Op::Create { dst, dir } = op {
            landings.push((dst.clone(), Landing::Create(*dir), None));
        }
    }
    let mut finals: Vec<(PathBuf, Landing, Option<PathBuf>)> =
        landings.into_iter().map(|(d, l, s)| (ctx.final_dst(&d, 0), l, s)).collect();
    finals.sort_by_key(|(d, _, _)| depth(d));
    for (dst, landing, orig) in finals {
        if let Some(parent) = dst.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                rep.errors.push(format!("mkdir {}: {e}", parent.display()));
                if let Landing::Staged(s) = &landing {
                    rep.errors.push(format!("  kept at {}", s.display()));
                }
                continue;
            }
        }
        let res = match &landing {
            Landing::Create(true) => fs::create_dir(&dst),
            Landing::Create(false) => fs::OpenOptions::new().write(true).create_new(true).open(&dst).map(|_| ()),
            Landing::Staged(stage) => {
                if fs::symlink_metadata(&dst).is_ok() {
                    Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination exists"))
                } else {
                    move_path(stage, &dst)
                }
            }
        };
        match res {
            Ok(()) => {
                rep.done += 1;
                rep.landed.push(dst);
            }
            Err(e) => {
                rep.errors.push(format!("{}: {e}", dst.display()));
                if let Landing::Staged(s) = landing {
                    // Try to put a moved entry back; else say where it is.
                    let back = orig.filter(|o| fs::symlink_metadata(o).is_err() && move_path(&s, o).is_ok());
                    match back {
                        Some(o) => rep.errors.push(format!("  restored {}", o.display())),
                        None => rep.errors.push(format!("  kept at {}", s.display())),
                    }
                }
            }
        }
    }
    rep
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{Buffer, Line};
    use crate::entry::{Sort, Store};
    use crate::plan::{build, tests::tmpdir};

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir).unwrap().map(|d| d.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    fn set(b: &mut Buffer, from: &str, to: &str) {
        b.lines.iter_mut().find(|l| l.text == from).unwrap().text = to.into();
    }

    #[test]
    fn swap_and_case_rename() {
        let t = tmpdir("apply-swap");
        fs::write(t.0.join("a"), "A").unwrap();
        fs::write(t.0.join("b"), "B").unwrap();
        fs::write(t.0.join("readme"), "R").unwrap();
        let mut store = Store::default();
        let mut b = Buffer::load(&t.0, &mut store, false, Sort::Name, false);
        set(&mut b, "a", "x");
        set(&mut b, "b", "a");
        set(&mut b, "x", "b");
        set(&mut b, "readme", "README");
        let p = build([&b], &store);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        let r = apply(&p.ops, DeleteMethod::Permanent);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(fs::read_to_string(t.0.join("a")).unwrap(), "B");
        assert_eq!(fs::read_to_string(t.0.join("b")).unwrap(), "A");
        assert!(names(&t.0).contains(&"README".to_string()));
    }

    #[test]
    fn rename_dir_and_edit_inside() {
        let t = tmpdir("apply-nested");
        fs::create_dir(t.0.join("sub")).unwrap();
        fs::write(t.0.join("sub/x"), "X").unwrap();
        fs::write(t.0.join("sub/gone"), "").unwrap();
        let mut store = Store::default();
        let mut root = Buffer::load(&t.0, &mut store, false, Sort::Name, false);
        let mut sub = Buffer::load(&t.0.join("sub"), &mut store, false, Sort::Name, false);
        set(&mut root, "sub/", "renamed/");
        set(&mut sub, "x", "y");
        sub.lines.retain(|l| l.text != "gone");
        sub.lines.push(Line::new("made/new.txt"));
        let p = build([&root, &sub], &store);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        let r = apply(&p.ops, DeleteMethod::Permanent);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(names(&t.0), ["renamed"]);
        assert_eq!(names(&t.0.join("renamed")), ["made", "y"]);
        assert_eq!(fs::read_to_string(t.0.join("renamed/y")).unwrap(), "X");
        assert!(t.0.join("renamed/made/new.txt").is_file());
    }

    #[test]
    fn move_child_out_then_delete_parent() {
        let t = tmpdir("apply-rescue");
        fs::create_dir(t.0.join("old")).unwrap();
        fs::write(t.0.join("old/keep"), "K").unwrap();
        let mut store = Store::default();
        let mut root = Buffer::load(&t.0, &mut store, false, Sort::Name, false);
        let mut old = Buffer::load(&t.0.join("old"), &mut store, false, Sort::Name, false);
        let keep = old.lines.remove(0);
        root.lines.retain(|l| l.text != "old/");
        root.lines.push(keep);
        let p = build([&root, &old], &store);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        let r = apply(&p.ops, DeleteMethod::Permanent);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(names(&t.0), ["keep"]);
    }

    #[test]
    fn copy_dir_recursive() {
        let t = tmpdir("apply-copy");
        fs::create_dir_all(t.0.join("d/e")).unwrap();
        fs::write(t.0.join("d/e/f"), "F").unwrap();
        std::os::unix::fs::symlink("e/f", t.0.join("d/link")).unwrap();
        let mut store = Store::default();
        let mut b = Buffer::load(&t.0, &mut store, false, Sort::Name, false);
        let mut l = b.lines[0].clone();
        l.text = "d2/".into();
        b.lines.push(l);
        let p = build([&b], &store);
        let r = apply(&p.ops, DeleteMethod::Permanent);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(fs::read_to_string(t.0.join("d2/e/f")).unwrap(), "F");
        assert_eq!(fs::read_link(t.0.join("d2/link")).unwrap(), PathBuf::from("e/f"));
    }

    #[test]
    fn no_staging_leftovers() {
        let t = tmpdir("apply-clean");
        fs::write(t.0.join("a"), "").unwrap();
        let mut store = Store::default();
        let mut b = Buffer::load(&t.0, &mut store, true, Sort::Name, false);
        set(&mut b, "a", "dir/a");
        let p = build([&b], &store);
        let r = apply(&p.ops, DeleteMethod::Permanent);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(names(&t.0), ["dir"]);
    }
}
