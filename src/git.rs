//! Git beyond `status`: diffs, log, file contents at a revision, staging.
//! Same precautions as the status column everywhere: hooks, fsmonitor,
//! external diff, textconv and the pager off, no optional locks, and nothing
//! at all in a repository that defines its own filters (`git add` and
//! `git restore` run clean / smudge filters).

use crate::entry;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `git` with the safety knobs, running in `dir`.
pub fn cmd(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "diff.external=",
        "-c",
        "core.pager=cat",
        "-c",
        "core.quotePath=false",
        "-c",
        "color.ui=false",
        // a repository's own config could turn on signature checks and
        // point them at any program
        "-c",
        "log.showSignature=false",
        "-c",
        "gpg.program=false",
        "-c",
        "gpg.ssh.program=false",
        "-c",
        "gpg.x509.program=false",
        "--no-optional-locks",
        "--no-pager",
    ])
    .current_dir(dir)
    .env("GIT_OPTIONAL_LOCKS", "0")
    .stdin(Stdio::null());
    c
}

/// The work tree of `dir` when git may safely run there.
pub fn guard(dir: &Path) -> Result<PathBuf, String> {
    let dir = std::iter::successors(Some(dir), |d| d.parent()).find(|d| d.is_dir()).ok_or("no such folder")?;
    let root = entry::work_tree(dir).ok_or("not in a git repository")?;
    if entry::repo_defines_filters(&root) {
        return Err("this repository defines its own filters — git commands are off here".into());
    }
    Ok(root)
}

/// `p` relative to the work tree `root` (both resolved, so /tmp and
/// /private/tmp agree; the file itself may be gone).
pub fn rel(root: &Path, p: &Path) -> Option<String> {
    let parent = p.parent()?;
    let parent = std::iter::successors(Some(parent), |d| d.parent()).find(|d| d.exists())?;
    let rest = p.strip_prefix(parent).ok()?;
    let full = std::fs::canonicalize(parent).ok()?.join(rest);
    let r = full.strip_prefix(root).ok()?.to_string_lossy().into_owned();
    Some(if r.is_empty() { ".".into() } else { r })
}

fn run(mut c: Command) -> Result<String, String> {
    let out = c.stderr(Stdio::piped()).output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("git failed").trim().to_string())
    }
}

pub fn head_exists(root: &Path) -> bool {
    cmd(root).args(["rev-parse", "--verify", "-q", "HEAD"]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// The empty tree's id (differs between sha1 and sha256 repositories).
pub fn empty_tree(root: &Path) -> String {
    let mut c = cmd(root);
    c.args(["hash-object", "-t", "tree", "/dev/null"]);
    run(c).map(|s| s.trim().to_string()).unwrap_or_else(|_| "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into())
}

/// The base to diff against: `HEAD`, or the empty tree before the first commit.
pub fn base(root: &Path) -> String {
    if head_exists(root) {
        "HEAD".into()
    } else {
        empty_tree(root)
    }
}

fn rels(root: &Path, paths: &[PathBuf]) -> Result<Vec<String>, String> {
    paths.iter().map(|p| rel(root, p).ok_or_else(|| format!("{}: outside the repository", p.display()))).collect()
}

/// `git add -A -- paths` (new, changed and deleted files alike).
pub fn stage(root: &Path, paths: &[PathBuf]) -> Result<(), String> {
    let mut c = cmd(root);
    c.args(["add", "-A", "--"]).args(rels(root, paths)?);
    run(c).map(|_| ())
}

/// Take paths out of the index again (`git restore --staged`; before the
/// first commit `git rm --cached`).
pub fn unstage(root: &Path, paths: &[PathBuf]) -> Result<(), String> {
    let mut c = cmd(root);
    if head_exists(root) {
        c.args(["restore", "--staged", "--"]);
    } else {
        c.args(["rm", "--cached", "-r", "-q", "--"]);
    }
    c.args(rels(root, paths)?);
    run(c).map(|_| ())
}

/// Discard unstaged work-tree changes (`git restore --worktree`): the file
/// goes back to what is staged. Untracked files are not touched.
pub fn restore(root: &Path, paths: &[PathBuf]) -> Result<(), String> {
    let mut c = cmd(root);
    c.args(["restore", "--worktree", "--"]).args(rels(root, paths)?);
    run(c).map(|_| ())
}

/// `git log --oneline --graph --decorate -n max -- path`, coloured.
pub fn log(path: &Path, max: usize) -> Result<Vec<String>, String> {
    let root = guard(path)?;
    let r = rel(&root, path).ok_or("outside the repository")?;
    let mut c = cmd(&root);
    c.args(["-c", "color.ui=always", "log", "--no-show-signature", "--oneline", "--graph", "--decorate", "--color=always", "--date=short", "--format=%C(yellow)%h%C(reset) %C(dim)%ad%C(reset) %s %C(blue)%an%C(reset)%C(auto)%d"])
        .arg(format!("-n{max}"))
        .args(["--", &r]);
    let out = run(c)?;
    Ok(out.lines().map(str::to_string).collect())
}

/// A file's bytes at `spec` (`HEAD:path`, `:path` for the index).
pub fn show(root: &Path, spec: &str) -> Option<Vec<u8>> {
    let out = cmd(root).args(["show", "--no-textconv", spec]).stderr(Stdio::null()).output().ok()?;
    out.status.success().then_some(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_unstage_restore() {
        let t = crate::plan::tests::tmpdir("git-stage");
        let g = |args: &[&str]| Command::new("git").args(args).current_dir(&t.0).output().unwrap();
        g(&["init", "-q"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "t"]);
        std::fs::write(t.0.join("a.txt"), "one\n").unwrap();
        let root = guard(&t.0).unwrap();
        // before the first commit
        stage(&root, &[t.0.join("a.txt")]).unwrap();
        assert_eq!(entry::git_status(&t.0)["a.txt"].letter, 'A');
        unstage(&root, &[t.0.join("a.txt")]).unwrap();
        assert_eq!(entry::git_status(&t.0)["a.txt"].letter, '?');
        stage(&root, &[t.0.join("a.txt")]).unwrap();
        g(&["commit", "-qm", "x"]);
        std::fs::write(t.0.join("a.txt"), "two\n").unwrap();
        assert!(entry::git_status(&t.0)["a.txt"].unstaged);
        restore(&root, &[t.0.join("a.txt")]).unwrap();
        assert_eq!(std::fs::read_to_string(t.0.join("a.txt")).unwrap(), "one\n");
        assert!(log(&t.0.join("a.txt"), 5).unwrap().iter().any(|l| l.contains('x')));
    }
}
