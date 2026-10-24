//! Colours (shared with herdr-git). Defaults follow the Monokai-Spectrum-ish palette; any
//! `[theme.custom]` keys in ~/.config/herdr/config.toml override them so the
//! pane matches the rest of herdr.

use ratatui::style::Color;

#[derive(Clone, Debug)]
pub struct Theme {
    pub accent: Color,
    pub text: Color,
    pub subtext: Color,
    pub dim: Color,
    pub surface: Color,
    pub selection: Color,
    pub green: Color,
    pub blue: Color,
    pub red: Color,
    pub yellow: Color,
    pub peach: Color,
    pub teal: Color,
    /// Terminal background, for blending diff tints (`[theme.custom]
    /// background`, default #222222).
    pub bg: Color,
    /// The terminal takes 24-bit colours (`$COLORTERM`); without it the diff
    /// view drops its tinted backgrounds.
    pub truecolor: bool,
}

fn hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Color::Rgb(
        ((v >> 16) & 0xff) as u8,
        ((v >> 8) & 0xff) as u8,
        (v & 0xff) as u8,
    ))
}

/// Mix `fg` into `bg` (both RGB; anything else returns `fg`).
pub fn blend(fg: Color, bg: Color, alpha: f32) -> Color {
    match (fg, bg) {
        (Color::Rgb(r, g, b), Color::Rgb(r2, g2, b2)) => {
            let m = |a: u8, b: u8| (a as f32 * alpha + b as f32 * (1.0 - alpha)).round() as u8;
            Color::Rgb(m(r, r2), m(g, g2), m(b, b2))
        }
        _ => fg,
    }
}

impl Default for Theme {
    fn default() -> Self {
        let t = |s| hex(s).unwrap();
        Theme {
            accent: t("#4eaab7"),
            text: t("#f7f1ff"),
            subtext: t("#b8b3bf"),
            dim: t("#6e6a86"),
            surface: t("#313244"),
            selection: t("#3a3a4a"),
            green: t("#7bd88f"),
            blue: t("#948ae3"),
            red: t("#fc618d"),
            yellow: t("#fce566"),
            peach: t("#fd9353"),
            teal: t("#5ad4e6"),
            bg: t("#222222"),
            truecolor: std::env::var("COLORTERM").is_ok_and(|v| matches!(v.as_str(), "truecolor" | "24bit")),
        }
    }
}

impl Theme {
    pub fn load() -> Theme {
        let mut th = Theme::default();
        let path = std::env::var("HERDR_CONFIG_PATH")
            .map(std::path::PathBuf::from)
            .ok()
            .or_else(|| {
                let home = std::env::var("HOME").ok()?;
                Some(std::path::PathBuf::from(home).join(".config/herdr/config.toml"))
            });
        let Some(path) = path else { return th };
        let Ok(text) = std::fs::read_to_string(path) else {
            return th;
        };
        let Ok(doc) = text.parse::<toml::Table>() else {
            return th;
        };
        let Some(custom) = doc
            .get("theme")
            .and_then(|t| t.get("custom"))
            .and_then(|c| c.as_table())
        else {
            return th;
        };
        let get = |k: &str| custom.get(k).and_then(|v| v.as_str()).and_then(hex);
        if let Some(c) = get("accent") {
            th.accent = c;
        }
        if let Some(c) = get("text") {
            th.text = c;
        }
        if let Some(c) = get("subtext0") {
            th.subtext = c;
        }
        if let Some(c) = get("overlay1") {
            th.dim = c;
        }
        if let Some(c) = get("surface1") {
            th.surface = c;
        }
        if let Some(c) = get("selection_bg") {
            th.selection = c;
        }
        if let Some(c) = get("green") {
            th.green = c;
        }
        if let Some(c) = get("blue") {
            th.blue = c;
        }
        if let Some(c) = get("red") {
            th.red = c;
        }
        if let Some(c) = get("yellow") {
            th.yellow = c;
        }
        if let Some(c) = get("peach") {
            th.peach = c;
        }
        if let Some(c) = get("teal") {
            th.teal = c;
        }
        if let Some(c) = get("background") {
            th.bg = c;
        }
        th
    }

    /// `c` laid over the background at `alpha` (0–1); `None` without truecolor.
    pub fn tint(&self, c: Color, alpha: f32) -> Option<Color> {
        if !self.truecolor {
            return None;
        }
        Some(blend(c, self.bg, alpha))
    }

}
