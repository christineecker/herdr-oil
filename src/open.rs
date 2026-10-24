//! Hand files to an editor: inside the popup (suspend + resume), in a herdr
//! split/tab next to the pane the popup was opened from, typed into that
//! pane when it idles at a shell prompt, or via the system opener.

use crate::config::{Config, OpenTarget};
use crate::launch::{self, herdr};
use std::path::{Path, PathBuf};

pub enum Outcome {
    /// Done; close the popup unless `stay_open`.
    Handed(String),
    /// Run this argv in the foreground of the popup terminal.
    Suspend(Vec<String>),
}

/// POSIX single-quote.
pub fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:,@%".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Split an editor setting like `code -w` or `nvim --clean` into argv.
pub fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut q: Option<char> = None;
    let mut any = false;
    for c in s.chars() {
        match (q, c) {
            (Some(x), c) if c == x => q = None,
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                q = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() || any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() || any {
        out.push(cur);
    }
    out
}

/// argv for opening `files` (line applies to a single file only).
pub fn editor_argv(editor: &str, files: &[PathBuf], line: Option<usize>) -> Vec<String> {
    let mut argv = split_words(editor);
    if argv.is_empty() {
        argv.push("nvim".into());
    }
    let base = Path::new(&argv[0]).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let line = line.filter(|_| files.len() == 1);
    match (base.as_str(), line) {
        ("vim" | "nvim" | "vi" | "nano" | "micro" | "emacs" | "emacsclient" | "kak" | "mvim", Some(l)) => {
            argv.push(format!("+{l}"));
            argv.extend(files.iter().map(|f| f.display().to_string()));
        }
        ("hx" | "helix" | "zed" | "subl", Some(l)) => argv.push(format!("{}:{l}", files[0].display())),
        ("code" | "cursor" | "codium" | "windsurf", Some(l)) => {
            argv.push("-g".into());
            argv.push(format!("{}:{l}", files[0].display()));
        }
        ("vim" | "nvim", None) if files.len() > 1 => {
            argv.push("-p".into());
            argv.extend(files.iter().map(|f| f.display().to_string()));
        }
        _ => argv.extend(files.iter().map(|f| f.display().to_string())),
    }
    argv
}

fn shell_line(argv: &[String]) -> String {
    argv.iter().map(|a| sh_quote(a)).collect::<Vec<_>>().join(" ")
}

pub fn system_open(p: &Path) -> Result<(), String> {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::process::Command::new(opener)
        .arg(p)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{opener}: {e}"))
}

pub fn open(files: &[PathBuf], line: Option<usize>, how: OpenTarget, cfg: &Config, origin: Option<&str>) -> Result<Outcome, String> {
    if files.is_empty() {
        return Err("nothing to open".into());
    }
    let argv = editor_argv(&cfg.editor(), files, line);
    // Split/tab/origin targets *type* the command into a shell. A control
    // character in a file name (\r, \x15, \x1b…) would be interpreted by the
    // shell's line editor, so refuse those targets outright.
    let typed = matches!(how, OpenTarget::Auto | OpenTarget::SplitRight | OpenTarget::SplitDown | OpenTarget::Tab);
    if typed && argv.iter().any(|a| a.chars().any(char::is_control)) {
        return Err("name contains control characters — use ^e to edit inside the popup".into());
    }
    let dir = files[0].parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/"));
    let name = files[0].file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let herdr_ok = launch::in_herdr();
    let mut how = how;
    if !herdr_ok && matches!(how, OpenTarget::Auto | OpenTarget::SplitRight | OpenTarget::SplitDown | OpenTarget::Tab) {
        how = OpenTarget::Popup;
    }
    if how == OpenTarget::Auto {
        how = match origin {
            Some(o) if launch::pane_is_idle_shell(o) => {
                herdr(&["pane", "run", o, &shell_line(&argv)])?;
                return Ok(Outcome::Handed(format!("opened {name} in the origin pane")));
            }
            _ => OpenTarget::SplitRight,
        };
    }
    match how {
        OpenTarget::Popup | OpenTarget::Auto | OpenTarget::View => Ok(Outcome::Suspend(argv)),
        OpenTarget::System => {
            for f in files {
                system_open(f)?;
            }
            Ok(Outcome::Handed(format!("opened {name} with the system opener")))
        }
        OpenTarget::SplitRight | OpenTarget::SplitDown => {
            let dirn = if how == OpenTarget::SplitDown { "down" } else { "right" };
            let d = dir.display().to_string();
            let mut args = vec!["pane", "split"];
            if let Some(o) = origin {
                args.push(o);
            }
            args.extend(["--direction", dirn, "--cwd", &d, "--focus"]);
            let out = herdr(&args)?;
            let id = launch::pane_id_in(&out).ok_or("herdr pane split: no pane id in reply")?;
            herdr(&["pane", "run", &id, &format!("exec {}", shell_line(&argv))])?;
            Ok(Outcome::Handed(format!("opened {name} in a split")))
        }
        OpenTarget::Tab => {
            let d = dir.display().to_string();
            let out = herdr(&["tab", "create", "--cwd", &d, "--label", &name, "--focus"])?;
            let id = launch::pane_id_in(&out).ok_or("herdr tab create: no pane id in reply")?;
            herdr(&["pane", "run", &id, &format!("exec {}", shell_line(&argv))])?;
            Ok(Outcome::Handed(format!("opened {name} in a new tab")))
        }
    }
}

/// Copy text to the clipboard (pbcopy / wl-copy / xclip).
pub fn clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    let cands: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["pbcopy"]]
    } else {
        &[&["wl-copy"], &["xclip", "-selection", "clipboard"], &["xsel", "-b"]]
    };
    for c in cands {
        if let Ok(mut ch) = std::process::Command::new(c[0]).args(&c[1..]).stdin(std::process::Stdio::piped()).spawn() {
            if let Some(mut si) = ch.stdin.take() {
                let _ = si.write_all(text.as_bytes());
            }
            let _ = ch.wait();
            return Ok(());
        }
    }
    Err("no clipboard tool found".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("/a/b.txt"), "/a/b.txt");
        assert_eq!(sh_quote("it's here"), "'it'\\''s here'");
        assert_eq!(split_words("code -w"), ["code", "-w"]);
        assert_eq!(split_words("'my editor' --x \"\""), ["my editor", "--x", ""]);
    }

    #[test]
    fn refuses_control_chars_when_typing() {
        let cfg = Config::default();
        let f = vec![PathBuf::from("/tmp/evil\rrm -rf ~")];
        assert!(open(&f, None, OpenTarget::SplitRight, &cfg, Some("w1:p1")).is_err());
        assert!(matches!(open(&f, None, OpenTarget::Popup, &cfg, None), Ok(Outcome::Suspend(_))));
    }

    #[test]
    fn line_args() {
        let f = vec![PathBuf::from("/a.rs")];
        assert_eq!(editor_argv("nvim", &f, Some(3)), ["nvim", "+3", "/a.rs"]);
        assert_eq!(editor_argv("hx", &f, Some(3)), ["hx", "/a.rs:3"]);
        assert_eq!(editor_argv("code -w", &f, Some(3)), ["code", "-w", "-g", "/a.rs:3"]);
        let two = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        assert_eq!(editor_argv("nvim", &two, Some(1)), ["nvim", "-p", "/a", "/b"]);
    }
}
