//! `:extract` and `:zip` / `:tar`. Extraction goes into a new folder next to
//! the archive (never over existing files), and only after every entry was
//! checked: no absolute paths, no `..`, no symlinks or hard links (which
//! could point a later entry outside the folder).

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

const TAR_EXTS: &[&str] = &[".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.bz2", ".tbz2", ".tar.zst", ".tar"];
const ZIP_EXTS: &[&str] = &[".zip", ".jar", ".whl", ".epub", ".nupkg"];

/// (stem, is_zip) when `name` looks like an archive.
fn archive_kind(name: &str) -> Option<(String, bool)> {
    let lower = name.to_lowercase();
    for (exts, zip) in [(TAR_EXTS, false), (ZIP_EXTS, true)] {
        if let Some(e) = exts.iter().find(|e| lower.ends_with(*e)) {
            return Some((name[..name.len() - e.len()].to_string(), zip));
        }
    }
    None
}

/// An archive member name that stays inside the target folder.
pub fn safe_entry(name: &str) -> bool {
    let p = Path::new(name);
    !name.is_empty() && !name.contains('\0') && p.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

fn output(c: &mut Command) -> Result<String, String> {
    let out = c.stdin(Stdio::null()).stderr(Stdio::piped()).output().map_err(|e| format!("{e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("failed").trim().to_string())
    }
}

/// Members of the archive, refusing links and anything that would land
/// outside the target.
fn check_members(path: &Path, zip: bool) -> Result<usize, String> {
    let (names, long) = if zip {
        (output(Command::new("unzip").arg("-Z1").arg(path))?, output(Command::new("unzip").arg("-Z").arg(path))?)
    } else {
        (output(Command::new("tar").arg("-tf").arg(path))?, output(Command::new("tar").arg("-tvf").arg(path))?)
    };
    let mut n = 0;
    for name in names.lines().filter(|l| !l.is_empty()) {
        if !safe_entry(name) {
            return Err(format!("unsafe member {name:?} — not extracted"));
        }
        n += 1;
    }
    // long listings start with the mode: l = symlink, h = hard link (tar)
    if long.lines().any(|l| l.starts_with('l') || l.starts_with('h')) || long.contains(" link to ") {
        return Err("archive contains links — not extracted".into());
    }
    Ok(n)
}

/// Unpack `path` into a new folder named after it (`name 2` when taken).
pub fn extract_archive(path: &Path) -> Result<PathBuf, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or("no file name")?;
    let (stem, zip) = archive_kind(&name).ok_or("not a zip or tar archive")?;
    let parent = path.parent().ok_or("no folder")?;
    check_members(path, zip)?;
    let dest = crate::apply::unique(parent, &stem);
    std::fs::create_dir(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let r = if zip {
        output(Command::new("unzip").args(["-q", "-n"]).arg(path).arg("-d").arg(&dest))
    } else {
        output(Command::new("tar").args(["-x", "--no-same-owner", "-f"]).arg(path).arg("-C").arg(&dest))
    };
    if let Err(e) = r {
        // leave nothing half-made behind when nothing came out
        let _ = std::fs::remove_dir(&dest);
        return Err(e);
    }
    Ok(dest)
}

/// Pack `entries` (all in `dir`) into `dir/name`; zip unless `tar`. A tar
/// name picks its compression from the extension (`.tar.gz` default).
pub fn create_archive(dir: &Path, name: &str, tar: bool, entries: &[PathBuf]) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.starts_with('.') && name.len() < 3 {
        return Err(format!("{name}: give a plain file name"));
    }
    let lower = name.to_lowercase();
    let name = if tar && !TAR_EXTS.iter().any(|e| lower.ends_with(e)) {
        format!("{name}.tar.gz")
    } else if !tar && !lower.ends_with(".zip") {
        format!("{name}.zip")
    } else {
        name.to_string()
    };
    let dst = dir.join(&name);
    if std::fs::symlink_metadata(&dst).is_ok() {
        return Err(format!("{name} already exists"));
    }
    // "./name" so a member called "-x" is never read as an option
    let members: Vec<String> = entries.iter().filter_map(|p| p.file_name()).map(|n| format!("./{}", n.to_string_lossy())).collect();
    let mut c;
    if tar {
        let l = name.to_lowercase();
        let flag = if l.ends_with(".tar.xz") || l.ends_with(".txz") {
            "-cJf"
        } else if l.ends_with(".tar.bz2") || l.ends_with(".tbz2") {
            "-cjf"
        } else if l.ends_with(".tar") {
            "-cf"
        } else if l.ends_with(".tar.zst") {
            "--zstd"
        } else {
            "-czf"
        };
        c = Command::new("tar");
        if flag == "--zstd" {
            c.args(["--zstd", "-cf"]);
        } else {
            c.arg(flag);
        }
        c.arg(&name).args(&members);
    } else {
        c = Command::new("zip");
        c.args(["-q", "-r", "-y"]).arg(&name).args(&members);
    }
    c.current_dir(dir);
    output(&mut c).map(|_| dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_names() {
        assert!(safe_entry("a/b.txt") && safe_entry("./x"));
        assert!(!safe_entry("../evil") && !safe_entry("/etc/passwd") && !safe_entry("a/../../b") && !safe_entry(""));
        assert_eq!(archive_kind("x.TAR.GZ"), Some(("x".into(), false)));
        assert_eq!(archive_kind("a.b.zip"), Some(("a.b".into(), true)));
        assert_eq!(archive_kind("notes.txt"), None);
    }

    #[test]
    fn tar_roundtrip_into_new_folder() {
        let t = crate::plan::tests::tmpdir("archive");
        std::fs::create_dir(t.0.join("docs")).unwrap();
        std::fs::write(t.0.join("docs/a.md"), "a").unwrap();
        std::fs::write(t.0.join("b.txt"), "b").unwrap();
        let p = create_archive(&t.0, "pack", true, &[t.0.join("docs"), t.0.join("b.txt")]).unwrap();
        assert_eq!(p, t.0.join("pack.tar.gz"));
        assert!(create_archive(&t.0, "pack.tar.gz", true, &[t.0.join("b.txt")]).is_err(), "never overwrites");
        // the stem is taken by nothing yet: lands in pack/
        let d = extract_archive(&p).unwrap();
        assert_eq!(d, t.0.join("pack"));
        assert_eq!(std::fs::read_to_string(d.join("docs/a.md")).unwrap(), "a");
        // again: a second folder, the first untouched
        let d2 = extract_archive(&p).unwrap();
        assert_eq!(d2, t.0.join("pack 2"));
    }

    #[test]
    fn refuses_links() {
        let t = crate::plan::tests::tmpdir("archive-link");
        std::os::unix::fs::symlink("/etc", t.0.join("l")).unwrap();
        let p = create_archive(&t.0, "l.tar", true, &[t.0.join("l")]).unwrap();
        let e = extract_archive(&p).unwrap_err();
        assert!(e.contains("links"), "{e}");
        assert!(!t.0.join("l 2").exists());
    }
}
