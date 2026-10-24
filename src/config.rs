//! Plugin options from `$(herdr plugin config-dir herdr-oil)/config.toml`.
//! Every key is optional; see config.example.toml.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenTarget {
    /// Read-only viewer inside the oil popup (highlighted, scrollable).
    View,
    /// Origin pane when it sits at a shell prompt, else a split to the right.
    Auto,
    /// Run the editor inside the popup, come back to the explorer after.
    Popup,
    SplitRight,
    SplitDown,
    Tab,
    /// macOS `open` / `xdg-open`.
    System,
}

impl OpenTarget {
    pub fn parse(s: &str) -> Option<OpenTarget> {
        Some(match s {
            "view" => OpenTarget::View,
            "auto" => OpenTarget::Auto,
            "popup" => OpenTarget::Popup,
            "split" | "split-right" | "vsplit" => OpenTarget::SplitRight,
            "split-down" | "hsplit" => OpenTarget::SplitDown,
            "tab" => OpenTarget::Tab,
            "system" => OpenTarget::System,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteMethod {
    Trash,
    Permanent,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub editor: Option<String>,
    pub open: OpenTarget,
    pub delete: DeleteMethod,
    pub show_hidden: bool,
    /// 0 = off, 1 = size column, 2 = perms + size + age, 3 = owner, xattrs, links.
    pub details: u8,
    pub preview: bool,
    pub icons: bool,
    pub git_status: bool,
    /// Ask for confirmation before applying (always on when anything is deleted).
    pub confirm: bool,
    /// Keep the popup open after handing a file to a split/tab.
    pub stay_open: bool,
    /// Finder: honour .gitignore & co.
    pub respect_ignore: bool,
    /// Finder: cap on walked entries.
    pub max_files: usize,
    pub state_dir: PathBuf,
    /// Diff viewer layout: auto (split from 160 columns), unified, split.
    pub diff_layout: crate::diffview::Layout,
    /// `[recipes]`: name → range commands (`"s/ /_/g"`, `["kebab", "prefix {02#}-"]`).
    pub recipes: Vec<(String, Vec<String>)>,
}

impl Default for Config {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        Config {
            editor: None,
            open: OpenTarget::View,
            delete: DeleteMethod::Trash,
            show_hidden: false,
            details: 1,
            preview: true,
            icons: true,
            git_status: true,
            confirm: true,
            stay_open: false,
            respect_ignore: true,
            max_files: 200_000,
            state_dir: PathBuf::from(home).join(".local/state/herdr-oil"),
            diff_layout: crate::diffview::Layout::Auto,
            recipes: vec![],
        }
    }
}

pub fn config_dir() -> Option<PathBuf> {
    std::env::var("HERDR_PLUGIN_CONFIG_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let out = std::process::Command::new(crate::launch::herdr_bin())
                .args(["plugin", "config-dir", crate::launch::PLUGIN_ID])
                .output()
                .ok()?;
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!s.is_empty()).then(|| PathBuf::from(s))
        })
}

impl Config {
    pub fn load() -> Config {
        let mut c = Config::default();
        if let Some(dir) = std::env::var("HERDR_PLUGIN_STATE_DIR").ok().filter(|s| !s.is_empty()) {
            c.state_dir = PathBuf::from(dir);
        }
        let Some(dir) = config_dir() else { return c };
        let Ok(text) = std::fs::read_to_string(dir.join("config.toml")) else {
            return c;
        };
        let Ok(doc) = text.parse::<toml::Table>() else {
            return c;
        };
        c.apply(&doc);
        c
    }

    pub fn apply(&mut self, doc: &toml::Table) {
        let s = |k: &str| doc.get(k).and_then(|v| v.as_str()).map(str::to_string);
        let b = |k: &str| doc.get(k).and_then(|v| v.as_bool());
        if let Some(v) = s("editor").filter(|v| !v.trim().is_empty()) {
            self.editor = Some(v);
        }
        if let Some(v) = s("open").as_deref().and_then(OpenTarget::parse) {
            self.open = v;
        }
        if let Some(v) = s("delete") {
            self.delete = if v == "permanent" { DeleteMethod::Permanent } else { DeleteMethod::Trash };
        }
        if let Some(v) = b("show_hidden") {
            self.show_hidden = v;
        }
        match doc.get("details") {
            Some(toml::Value::Boolean(v)) => self.details = if *v { 2 } else { 0 },
            Some(toml::Value::String(v)) => {
                self.details = match v.as_str() {
                    "off" | "none" => 0,
                    "full" | "all" => 2,
                    "meta" | "metadata" => 3,
                    _ => 1,
                }
            }
            _ => {}
        }
        if let Some(v) = b("preview") {
            self.preview = v;
        }
        if let Some(v) = b("icons") {
            self.icons = v;
        }
        if let Some(v) = b("git_status") {
            self.git_status = v;
        }
        if let Some(v) = b("confirm") {
            self.confirm = v;
        }
        if let Some(v) = b("stay_open") {
            self.stay_open = v;
        }
        if let Some(v) = b("respect_ignore") {
            self.respect_ignore = v;
        }
        if let Some(v) = doc.get("max_files").and_then(|v| v.as_integer()) {
            self.max_files = v.max(1000) as usize;
        }
        if let Some(v) = s("diff_layout").as_deref().and_then(crate::diffview::Layout::parse) {
            self.diff_layout = v;
        }
        if let Some(t) = doc.get("recipes").and_then(|v| v.as_table()) {
            self.recipes = t
                .iter()
                .filter_map(|(k, v)| {
                    let steps: Vec<String> = match v {
                        toml::Value::String(s) => vec![s.clone()],
                        toml::Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
                        _ => return None,
                    };
                    (!steps.is_empty()).then(|| (k.clone(), steps))
                })
                .collect();
        }
    }

    /// Editor command line: config `editor`, else $VISUAL, $EDITOR, nvim.
    pub fn editor(&self) -> String {
        self.editor
            .clone()
            .or_else(|| std::env::var("VISUAL").ok().filter(|s| !s.trim().is_empty()))
            .or_else(|| std::env::var("EDITOR").ok().filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| "nvim".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_options() {
        let mut c = Config::default();
        let doc: toml::Table = r#"
            open = "tab"
            delete = "permanent"
            show_hidden = true
            editor = "hx"
            max_files = 5
        "#
        .parse()
        .unwrap();
        c.apply(&doc);
        assert_eq!(c.open, OpenTarget::Tab);
        assert_eq!(c.delete, DeleteMethod::Permanent);
        assert!(c.show_hidden);
        assert_eq!(c.editor(), "hx");
        assert_eq!(c.max_files, 1000);
        assert_eq!(c.details, 1);
        c.apply(&"details = true".parse().unwrap());
        assert_eq!(c.details, 2);
        c.apply(&"details = \"off\"".parse().unwrap());
        assert_eq!(c.details, 0);
        c.apply(&"diff_layout = \"split\"\n[recipes]\nphotos = [\"kebab\", \"prefix {02#}-\"]\nunder = \"s/ /_/g\"".parse().unwrap());
        assert_eq!(c.diff_layout, crate::diffview::Layout::Split);
        assert_eq!(c.recipes.iter().find(|(k, _)| k == "photos").map(|(_, v)| v.len()), Some(2));
    }
}
