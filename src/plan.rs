//! Turn edited buffers into filesystem operations (oil.nvim's diff step).
//!
//! For every entry id listed in some buffer we look at where its id appears
//! now, across *all* buffers:
//!   - nowhere                       → delete
//!   - at its original path (+ more) → keep, every other spot is a copy
//!   - elsewhere only                → the first spot is a move, the rest copies
//! Lines without an id are creates (`name/` → directory, `a/b/c` → nested,
//! `name -> target` → symlink). Changing the `-> target` of a listed
//! symlink retargets it.

use crate::buffer::Buffer;
use crate::entry::Store;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Create { dst: PathBuf, dir: bool },
    Move { src: PathBuf, dst: PathBuf, dir: bool },
    Copy { src: PathBuf, dst: PathBuf, dir: bool },
    Delete { src: PathBuf, dir: bool },
    /// New symlink at `dst` pointing to `target`.
    Link { dst: PathBuf, target: String },
    /// Point the symlink at `path` (where it ends up after moves) elsewhere.
    Retarget { path: PathBuf, target: String },
}

impl Op {
    pub fn src(&self) -> Option<&Path> {
        match self {
            Op::Move { src, .. } | Op::Copy { src, .. } | Op::Delete { src, .. } => Some(src),
            Op::Create { .. } | Op::Link { .. } | Op::Retarget { .. } => None,
        }
    }
    pub fn dst(&self) -> Option<&Path> {
        match self {
            Op::Move { dst, .. } | Op::Copy { dst, .. } | Op::Create { dst, .. } | Op::Link { dst, .. } => Some(dst),
            Op::Delete { .. } | Op::Retarget { .. } => None,
        }
    }
    pub fn is_dir(&self) -> bool {
        match self {
            Op::Create { dir, .. } | Op::Move { dir, .. } | Op::Copy { dir, .. } | Op::Delete { dir, .. } => *dir,
            Op::Link { .. } | Op::Retarget { .. } => false,
        }
    }
    pub fn rank(&self) -> u8 {
        match self {
            Op::Create { .. } | Op::Link { .. } => 0,
            Op::Move { .. } => 1,
            Op::Copy { .. } => 2,
            Op::Retarget { .. } => 3,
            Op::Delete { .. } => 4,
        }
    }
}

#[derive(Debug, Default)]
pub struct Plan {
    pub ops: Vec<Op>,
    pub errors: Vec<String>,
}

impl Plan {
    pub fn deletes(&self) -> usize {
        self.ops.iter().filter(|o| matches!(o, Op::Delete { .. })).count()
    }
}

/// Filesystem-identity key: APFS/HFS+ are case-insensitive by default.
pub fn key(p: &Path) -> String {
    let s = p.to_string_lossy();
    if cfg!(target_os = "macos") {
        s.to_lowercase()
    } else {
        s.into_owned()
    }
}

/// Lexically normalise `.`/`..` (no filesystem access).
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Split `name -> target` (a symlink line) into its name and target.
pub fn split_link(text: &str) -> (&str, Option<&str>) {
    match text.split_once(" -> ") {
        Some((a, b)) => (a, Some(b.trim())),
        None => (text, None),
    }
}

/// Parse a buffer line into (relative path, wants-dir). `None` for blank.
pub fn parse_name(text: &str) -> Result<Option<(PathBuf, bool)>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    if t.contains('\0') {
        return Err(format!("invalid name {t:?}"));
    }
    if t.starts_with('/') || t.starts_with('~') {
        return Err(format!("{t}: absolute paths are not allowed, use a relative name"));
    }
    let dir = t.ends_with('/');
    let core = t.trim_end_matches('/');
    let mut rel = PathBuf::new();
    for part in core.split('/') {
        match part {
            "" | "." => continue,
            p => rel.push(p),
        }
    }
    if rel.as_os_str().is_empty() || rel.components().all(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("{t}: not a valid name"));
    }
    Ok(Some((rel, dir)))
}

pub fn build<'a>(buffers: impl IntoIterator<Item = &'a Buffer>, store: &Store) -> Plan {
    let mut bufs: Vec<&Buffer> = buffers.into_iter().collect();
    bufs.sort_by(|a, b| a.dir.cmp(&b.dir));
    let mut plan = Plan::default();
    // id → destinations (in buffer order), with a `-> target` if given
    let mut occ: HashMap<u64, Vec<(PathBuf, Option<String>)>> = HashMap::new();
    let mut creates: Vec<(PathBuf, bool, Option<String>)> = Vec::new();

    for b in &bufs {
        for (i, line) in b.lines.iter().enumerate() {
            if !line.significant() {
                continue;
            }
            let (name, target) = split_link(&line.text);
            if target == Some("") {
                plan.errors.push(format!("{}: line {}: missing symlink target after ->", short(&b.dir), i + 1));
                continue;
            }
            let target = target.map(str::to_string);
            let parsed = match parse_name(name) {
                Ok(Some(p)) => p,
                Ok(None) => {
                    if line.id.is_some() {
                        plan.errors.push(format!(
                            "{}: line {} is empty — delete the line (dd) to remove the entry",
                            short(&b.dir),
                            i + 1
                        ));
                    }
                    continue;
                }
                Err(e) => {
                    plan.errors.push(format!("{}: line {}: {e}", short(&b.dir), i + 1));
                    continue;
                }
            };
            let dst = normalize(&b.dir.join(&parsed.0));
            match line.id {
                Some(id) if store.get(id).is_some() => {
                    let e = store.get(id).unwrap();
                    if target.is_some() && e.link_target.is_none() {
                        plan.errors.push(format!("{}: line {}: {} is not a symlink — `-> target` only works on links and new lines", short(&b.dir), i + 1, e.name));
                        continue;
                    }
                    occ.entry(id).or_default().push((dst, target))
                }
                _ => creates.push((dst, parsed.1, target)),
            }
        }
    }

    let mut seen_ids = HashSet::new();
    for b in &bufs {
        for &id in &b.original {
            let Some(e) = store.get(id) else { continue };
            seen_ids.insert(id);
            let dir = e.kind.is_dir();
            let src = e.path.clone();
            let spots = occ.get(&id).cloned().unwrap_or_default();
            if spots.is_empty() {
                plan.ops.push(Op::Delete { src, dir });
                continue;
            }
            retargets(&mut plan, e.link_target.as_deref(), &spots);
            if spots.iter().any(|(d, _)| *d == src) {
                for (d, _) in spots.into_iter().filter(|(d, _)| *d != src) {
                    plan.ops.push(Op::Copy { src: src.clone(), dst: d, dir });
                }
                continue;
            }
            // Prefer a destination in the same parent (a rename) for the move.
            let parent = src.parent().map(Path::to_path_buf);
            let mi = spots.iter().position(|(d, _)| d.parent().map(Path::to_path_buf) == parent).unwrap_or(0);
            for (i, (d, _)) in spots.into_iter().enumerate() {
                if i == mi {
                    plan.ops.push(Op::Move { src: src.clone(), dst: d, dir });
                } else {
                    plan.ops.push(Op::Copy { src: src.clone(), dst: d, dir });
                }
            }
        }
    }
    // Ids pasted from a buffer that is no longer loaded: copies.
    let mut orphans: Vec<_> = occ.iter().filter(|(id, _)| !seen_ids.contains(id)).collect();
    orphans.sort_by_key(|(id, _)| **id);
    for (id, spots) in orphans {
        let e = store.get(*id).unwrap();
        retargets(&mut plan, e.link_target.as_deref(), spots);
        for (d, _) in spots {
            if *d != e.path {
                plan.ops.push(Op::Copy { src: e.path.clone(), dst: d.clone(), dir: e.kind.is_dir() });
            }
        }
    }
    for (dst, dir, target) in creates {
        match target {
            Some(target) => plan.ops.push(Op::Link { dst, target }),
            None => plan.ops.push(Op::Create { dst, dir }),
        }
    }
    plan.ops.sort_by_key(Op::rank);
    validate(&mut plan);
    plan
}

/// A symlink line whose `-> target` changed: retarget it where it lands.
fn retargets(plan: &mut Plan, old: Option<&str>, spots: &[(PathBuf, Option<String>)]) {
    for (d, t) in spots {
        if let Some(t) = t.as_deref().filter(|t| Some(*t) != old) {
            plan.ops.push(Op::Retarget { path: d.clone(), target: t.to_string() });
        }
    }
}

fn short(p: &Path) -> String {
    let s = p.display().to_string();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

fn validate(plan: &mut Plan) {
    let vacated: HashSet<String> = plan
        .ops
        .iter()
        .filter(|o| matches!(o, Op::Move { .. } | Op::Delete { .. }))
        .filter_map(|o| o.src().map(key))
        .collect();
    let mut dsts: HashMap<String, &Path> = HashMap::new();
    let mut errs = Vec::new();
    for op in &plan.ops {
        let Some(dst) = op.dst() else { continue };
        let k = key(dst);
        if dsts.insert(k.clone(), dst).is_some() {
            errs.push(format!("{}: two entries would get this name", short(dst)));
            continue;
        }
        if std::fs::symlink_metadata(dst).is_ok() && !vacated.contains(&k) {
            errs.push(format!("{}: already exists", short(dst)));
        }
        if let (Some(src), true) = (op.src(), op.is_dir()) {
            if dst != src && dst.starts_with(src) {
                errs.push(format!("{}: cannot move or copy a directory into itself", short(dst)));
            }
        }
        if dst.components().any(|c| matches!(c, Component::ParentDir)) || !dst.is_absolute() {
            errs.push(format!("{}: resolves outside the filesystem root", short(dst)));
        }
    }
    errs.dedup();
    plan.errors.extend(errs);
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::buffer::Line;
    use crate::entry::Sort;
    use std::fs;

    pub struct Tmp(pub PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    pub fn tmpdir(tag: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!("herdr-oil-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(fs::canonicalize(&p).unwrap())
    }

    fn setup(tag: &str) -> (Tmp, Store, Buffer) {
        let t = tmpdir(tag);
        fs::write(t.0.join("a.txt"), "a").unwrap();
        fs::write(t.0.join("b.txt"), "b").unwrap();
        fs::create_dir(t.0.join("sub")).unwrap();
        let mut store = Store::default();
        let b = Buffer::load(&t.0, &mut store, false, Sort::Name, false);
        (t, store, b)
    }

    fn line_of<'a>(b: &'a mut Buffer, text: &str) -> &'a mut Line {
        b.lines.iter_mut().find(|l| l.text == text).unwrap()
    }

    #[test]
    fn unchanged_is_empty() {
        let (_t, store, b) = setup("noop");
        let p = build([&b], &store);
        assert!(p.ops.is_empty(), "{:?}", p.ops);
        assert!(p.errors.is_empty());
    }

    #[test]
    fn rename_delete_create() {
        let (t, store, mut b) = setup("rdc");
        line_of(&mut b, "a.txt").text = "c.txt".into();
        b.lines.retain(|l| l.text != "b.txt");
        b.lines.push(Line::new("new/"));
        b.lines.push(Line::new("deep/er/file.rs"));
        let p = build([&b], &store);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        let r = &t.0;
        assert_eq!(
            p.ops,
            vec![
                Op::Create { dst: r.join("new"), dir: true },
                Op::Create { dst: r.join("deep/er/file.rs"), dir: false },
                Op::Move { src: r.join("a.txt"), dst: r.join("c.txt"), dir: false },
                Op::Delete { src: r.join("b.txt"), dir: false },
            ]
        );
    }

    #[test]
    fn duplicate_line_is_copy() {
        let (t, store, mut b) = setup("copy");
        let mut l = line_of(&mut b, "a.txt").clone();
        l.text = "a2.txt".into();
        b.lines.push(l);
        let p = build([&b], &store);
        assert_eq!(p.ops, vec![Op::Copy { src: t.0.join("a.txt"), dst: t.0.join("a2.txt"), dir: false }]);
    }

    #[test]
    fn cut_paste_across_buffers_is_move() {
        let (t, mut store, mut b) = setup("xbuf");
        let mut sub = Buffer::load(&t.0.join("sub"), &mut store, false, Sort::Name, false);
        let l = line_of(&mut b, "a.txt").clone();
        b.lines.retain(|x| x.text != "a.txt");
        sub.lines = vec![l];
        let p = build([&b, &sub], &store);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert_eq!(p.ops, vec![Op::Move { src: t.0.join("a.txt"), dst: t.0.join("sub/a.txt"), dir: false }]);
    }

    #[test]
    fn swap_names_is_allowed() {
        let (t, store, mut b) = setup("swap");
        line_of(&mut b, "a.txt").text = "tmp".into();
        line_of(&mut b, "b.txt").text = "a.txt".into();
        line_of(&mut b, "tmp").text = "b.txt".into();
        let p = build([&b], &store);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert_eq!(p.ops.len(), 2);
        let _ = t;
    }

    #[test]
    fn conflicts_are_errors() {
        let (_t, store, mut b) = setup("conflict");
        line_of(&mut b, "a.txt").text = "b.txt".into();
        let p = build([&b], &store);
        assert!(p.errors.iter().any(|e| e.contains("already exists")), "{:?}", p.errors);

        let (_t2, store2, mut b2) = setup("dupe");
        b2.lines.push(Line::new("x"));
        b2.lines.push(Line::new("x"));
        let p2 = build([&b2], &store2);
        assert!(p2.errors.iter().any(|e| e.contains("two entries")), "{:?}", p2.errors);
    }

    #[test]
    fn dir_into_itself_is_error() {
        let (_t, store, mut b) = setup("self");
        line_of(&mut b, "sub/").text = "sub/inner/".into();
        let p = build([&b], &store);
        assert!(p.errors.iter().any(|e| e.contains("into itself")), "{:?}", p.errors);
    }

    #[test]
    fn names() {
        assert_eq!(parse_name("  ").unwrap(), None);
        assert_eq!(parse_name("x/").unwrap(), Some((PathBuf::from("x"), true)));
        assert_eq!(parse_name("a//b/./c").unwrap(), Some((PathBuf::from("a/b/c"), false)));
        assert!(parse_name("/etc").is_err());
        assert!(parse_name("..").is_err());
        assert_eq!(parse_name("../up.txt").unwrap(), Some((PathBuf::from("../up.txt"), false)));
        assert_eq!(normalize(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
    }

    #[test]
    fn emptied_line_is_error() {
        let (_t, store, mut b) = setup("blank");
        line_of(&mut b, "a.txt").text = "".into();
        let p = build([&b], &store);
        assert!(p.errors.iter().any(|e| e.contains("is empty")), "{:?}", p.errors);
    }
}
