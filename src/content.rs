//! The explorer's content filter (`g/pattern`): ripgrep match counts for
//! each entry of a listed folder, a folder's count being the total of
//! everything under it. Counted on a thread per folder; the query uses the
//! grep finder's syntax (`foo -- *.rs -t py`).

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

type Counts = HashMap<String, usize>;

pub struct Filter {
    pub query: String,
    /// `zm`: rows without hits are left out of the listing (`zr` shows them).
    pub only: bool,
    counts: HashMap<PathBuf, Counts>,
    pending: HashSet<PathBuf>,
    tx: Sender<(PathBuf, Counts)>,
    rx: Receiver<(PathBuf, Counts)>,
}

impl Filter {
    pub fn new(query: String) -> Filter {
        let (tx, rx) = channel();
        Filter { query, only: false, counts: HashMap::new(), pending: HashSet::new(), tx, rx }
    }

    /// Count the hits under `dir` unless that is done or under way.
    pub fn request(&mut self, dir: &Path, hidden: bool) {
        if self.counts.contains_key(dir) || !self.pending.insert(dir.to_path_buf()) {
            return;
        }
        let (tx, q, dir) = (self.tx.clone(), self.query.clone(), dir.to_path_buf());
        std::thread::spawn(move || {
            let c = count(&q, &dir, hidden);
            let _ = tx.send((dir, c));
        });
    }

    /// Drop `dir`'s counts (the folder changed); the next request recounts.
    pub fn forget(&mut self, dir: &Path) {
        self.counts.remove(dir);
    }

    /// Take finished counts; the folders that got them.
    pub fn poll(&mut self) -> Vec<PathBuf> {
        let mut got = vec![];
        while let Ok((d, c)) = self.rx.try_recv() {
            self.pending.remove(&d);
            self.counts.insert(d.clone(), c);
            got.push(d);
        }
        got
    }

    pub fn counts(&self, dir: &Path) -> Option<&Counts> {
        self.counts.get(dir)
    }

    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// Hits per entry of `dir`: the per-file counts summed by first component.
pub fn count(q: &str, dir: &Path, hidden: bool) -> Counts {
    let mut out = Counts::new();
    let Some(mut cmd) = crate::finder::grep_cmd(q, dir, hidden, false, true) else { return out };
    let Ok(mut child) = cmd.spawn() else { return out };
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Some((path, n)) = line.rsplit_once(':') else { continue };
            let Ok(n) = n.parse::<usize>() else { continue };
            let path = path.strip_prefix("./").unwrap_or(path);
            if let (true, Some(name)) = (n > 0, path.split('/').next()) {
                *out.entry(name.to_string()).or_default() += n;
            }
        }
    }
    let _ = child.wait();
    out
}

/// Up to `max` hits under `dir`, for a folder's preview.
pub fn hits(q: &str, dir: &Path, hidden: bool, max: usize) -> Vec<crate::finder::Hit> {
    let Some(mut cmd) = crate::finder::grep_cmd(q, dir, hidden, false, false) else { return vec![] };
    let Ok(mut child) = cmd.spawn() else { return vec![] };
    let mut out = vec![];
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(h) = crate::finder::parse_hit(&line, dir) {
                out.push(h);
            }
            if out.len() >= max {
                break;
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_per_entry() {
        let t = crate::plan::tests::tmpdir("content");
        std::fs::create_dir_all(t.0.join("src/deep")).unwrap();
        std::fs::write(t.0.join("src/deep/a.rs"), "hay hay\nhay\n").unwrap();
        std::fs::write(t.0.join("src/b.rs"), "hay\n").unwrap();
        std::fs::write(t.0.join("top.txt"), "hay\n").unwrap();
        std::fs::write(t.0.join("none.txt"), "straw\n").unwrap();
        let c = count("hay", &t.0, false);
        // rg counts matches, grep counts lines
        let deep = if c.get("src") == Some(&4) { 4 } else { 3 };
        assert_eq!(c.get("src"), Some(&deep), "{c:?}");
        assert_eq!(c.get("top.txt"), Some(&1));
        assert!(!c.contains_key("none.txt"));
        assert!(count("hay -- *.txt", &t.0, false).keys().eq(["top.txt"].iter()));
        let mut f = Filter::new("hay".into());
        f.request(&t.0, false);
        let t0 = std::time::Instant::now();
        while f.busy() && t0.elapsed() < std::time::Duration::from_secs(5) {
            f.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(f.counts(&t.0).and_then(|c| c.get("top.txt")), Some(&1));
        let h = hits("hay", &t.0.join("src"), false, 10);
        assert_eq!(h.len(), 3);
    }
}
