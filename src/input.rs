//! One-line text input shared by the finder query, the `:` / `/` command
//! line and the viewer's `/` prompt, so the readline keys behave the same
//! everywhere: `^a` / `Home`, `^e` / `alt-e` / `End`, `Left` / `Right`,
//! `alt-b` / `alt-f` by word, `^w` / `alt-⌫` delete a word, `^u` to the
//! start, `^k` to the end, `Delete`, `Backspace`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What a key did to the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Not a line-editing key; the caller handles it.
    Ignored,
    /// Only the cursor moved.
    Moved,
    /// The text changed; `true` when it was a plain append at the end.
    Changed(bool),
}

fn word_start(v: &[char], cur: usize) -> usize {
    let mut a = cur;
    while a > 0 && v[a - 1].is_whitespace() {
        a -= 1;
    }
    let word = |c: char| c.is_alphanumeric() || c == '_';
    if a > 0 && word(v[a - 1]) {
        while a > 0 && word(v[a - 1]) {
            a -= 1;
        }
    } else {
        while a > 0 && !v[a - 1].is_whitespace() && !word(v[a - 1]) {
            a -= 1;
        }
    }
    a
}

fn word_end(v: &[char], cur: usize) -> usize {
    let mut e = cur;
    while e < v.len() && v[e].is_whitespace() {
        e += 1;
    }
    while e < v.len() && !v[e].is_whitespace() {
        e += 1;
    }
    e
}

/// Apply `k` to `text` with the cursor at char index `cur`.
pub fn edit(text: &mut String, cur: &mut usize, k: KeyEvent) -> Edit {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let mut v: Vec<char> = text.chars().collect();
    *cur = (*cur).min(v.len());
    let at_end = *cur == v.len();
    let r = match k.code {
        KeyCode::Char('a') if ctrl => {
            *cur = 0;
            Edit::Moved
        }
        KeyCode::Home => {
            *cur = 0;
            Edit::Moved
        }
        KeyCode::Char('e') if ctrl || alt => {
            *cur = v.len();
            Edit::Moved
        }
        KeyCode::End => {
            *cur = v.len();
            Edit::Moved
        }
        KeyCode::Left if ctrl || alt => {
            *cur = word_start(&v, *cur);
            Edit::Moved
        }
        KeyCode::Right if ctrl || alt => {
            *cur = word_end(&v, *cur);
            Edit::Moved
        }
        KeyCode::Char('b') if alt => {
            *cur = word_start(&v, *cur);
            Edit::Moved
        }
        KeyCode::Char('f') if alt => {
            *cur = word_end(&v, *cur);
            Edit::Moved
        }
        KeyCode::Left => {
            *cur = cur.saturating_sub(1);
            Edit::Moved
        }
        KeyCode::Right => {
            *cur = (*cur + 1).min(v.len());
            Edit::Moved
        }
        KeyCode::Char('w') if ctrl => {
            let a = word_start(&v, *cur);
            v.drain(a..*cur);
            *cur = a;
            Edit::Changed(false)
        }
        KeyCode::Backspace if alt || ctrl => {
            let a = word_start(&v, *cur);
            v.drain(a..*cur);
            *cur = a;
            Edit::Changed(false)
        }
        KeyCode::Char('u') if ctrl => {
            v.drain(..*cur);
            *cur = 0;
            Edit::Changed(false)
        }
        KeyCode::Char('k') if ctrl => {
            v.truncate(*cur);
            Edit::Changed(false)
        }
        KeyCode::Backspace => {
            if *cur == 0 {
                return Edit::Ignored;
            }
            v.remove(*cur - 1);
            *cur -= 1;
            Edit::Changed(false)
        }
        KeyCode::Delete => {
            if *cur < v.len() {
                v.remove(*cur);
            }
            Edit::Changed(false)
        }
        KeyCode::Char(c) if !ctrl && !alt => {
            v.insert(*cur, c);
            *cur += 1;
            Edit::Changed(at_end)
        }
        _ => Edit::Ignored,
    };
    if matches!(r, Edit::Changed(_)) {
        *text = v.into_iter().collect();
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn readline_keys() {
        let (mut t, mut c) = (String::from("foo bar.baz"), 11);
        assert_eq!(edit(&mut t, &mut c, k(KeyCode::Char('w'), KeyModifiers::CONTROL)), Edit::Changed(false));
        assert_eq!((t.as_str(), c), ("foo bar.", 8));
        edit(&mut t, &mut c, k(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(c, 0);
        assert_eq!(edit(&mut t, &mut c, k(KeyCode::Char('x'), KeyModifiers::NONE)), Edit::Changed(false));
        assert_eq!(t, "xfoo bar.");
        edit(&mut t, &mut c, k(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(c, 9);
        assert_eq!(edit(&mut t, &mut c, k(KeyCode::Char('!'), KeyModifiers::NONE)), Edit::Changed(true));
        edit(&mut t, &mut c, k(KeyCode::Char('b'), KeyModifiers::ALT));
        edit(&mut t, &mut c, k(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(t, "xfoo bar");
        edit(&mut t, &mut c, k(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(edit(&mut t, &mut c, k(KeyCode::Backspace, KeyModifiers::NONE)), Edit::Ignored);
        edit(&mut t, &mut c, k(KeyCode::Right, KeyModifiers::NONE));
        edit(&mut t, &mut c, k(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!((t.as_str(), c), ("foo bar", 0));
        assert_eq!(edit(&mut t, &mut c, k(KeyCode::Enter, KeyModifiers::NONE)), Edit::Ignored);
    }
}
