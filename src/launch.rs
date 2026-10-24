//! Launcher (`--launch explorer|finder|grep`): read the focused pane from
//! `herdr pane list`, then open the popup with that pane's cwd and id passed
//! through `--env` (never `--cwd`: the relative command must resolve from
//! the plugin root).

use serde::Deserialize;
use std::process::Command;

pub const PLUGIN_ID: &str = "herdr-oil";
pub const ENTRYPOINT: &str = "oil";

#[derive(Deserialize)]
struct PaneListMsg {
    result: PaneListResult,
}
#[derive(Deserialize)]
struct PaneListResult {
    panes: Vec<Pane>,
}
#[derive(Deserialize, Default, Clone)]
pub struct Pane {
    pub pane_id: Option<String>,
    #[serde(default)]
    pub focused: bool,
    pub cwd: Option<String>,
    pub foreground_cwd: Option<String>,
    pub agent: Option<String>,
}

pub fn flag_safe(id: &str) -> bool {
    !id.is_empty() && !id.starts_with('-') && id.chars().all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '_' || c == '-')
}

pub fn herdr_bin() -> String {
    std::env::var("HERDR_BIN_PATH").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "herdr".into())
}

pub fn in_herdr() -> bool {
    std::env::var("HERDR_SOCKET_PATH").is_ok() || std::env::var("HERDR_PANE_ID").is_ok() || std::env::var("HERDR_BIN_PATH").is_ok()
}

pub fn herdr(args: &[&str]) -> Result<String, String> {
    let out = Command::new(herdr_bin()).args(args).output().map_err(|e| format!("herdr: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn panes() -> Vec<Pane> {
    let Ok(list) = herdr(&["pane", "list"]) else { return vec![] };
    serde_json::from_str::<PaneListMsg>(list.trim_start_matches('\u{feff}')).map(|m| m.result.panes).unwrap_or_default()
}

pub fn focused(panes: &[Pane]) -> Option<Pane> {
    panes.iter().find(|p| p.focused).cloned()
}

pub fn run(mode: &str) -> Result<(), String> {
    let ps = panes();
    let f = focused(&ps).unwrap_or_default();
    let cwd = f
        .foreground_cwd
        .or(f.cwd)
        .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
        .unwrap_or_default();
    let mut args: Vec<String> = ["plugin", "pane", "open", "--plugin", PLUGIN_ID, "--entrypoint", ENTRYPOINT, "--placement", "popup", "--focus"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.push("--env".into());
    args.push(format!("HERDR_OIL_ROOT={cwd}"));
    args.push("--env".into());
    args.push(format!("HERDR_OIL_MODE={mode}"));
    if let Some(id) = f.pane_id.filter(|id| flag_safe(id)) {
        args.push("--env".into());
        args.push(format!("HERDR_OIL_ORIGIN={id}"));
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    herdr(&refs).map(|_| ())
}

/// Find a pane id anywhere in a herdr JSON reply (`result.pane.pane_id`,
/// `result.root_pane.pane_id`, …).
pub fn pane_id_in(json: &str) -> Option<String> {
    fn walk(v: &serde_json::Value) -> Option<String> {
        match v {
            serde_json::Value::Object(m) => {
                for k in ["pane", "root_pane", "plugin_pane"] {
                    if let Some(x) = m.get(k).and_then(walk) {
                        return Some(x);
                    }
                }
                m.get("pane_id").and_then(|x| x.as_str()).map(str::to_string).or_else(|| m.values().find_map(walk))
            }
            _ => None,
        }
    }
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    walk(v.get("result")?).filter(|id| flag_safe(id))
}

/// Is the pane sitting at an interactive shell prompt (no agent, only a
/// shell in the foreground)? Only then do we type into it.
pub fn pane_is_idle_shell(id: &str) -> bool {
    if panes().iter().any(|p| p.pane_id.as_deref() == Some(id) && p.agent.is_some()) {
        return false;
    }
    let Ok(out) = herdr(&["pane", "process-info", "--pane", id]) else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) else { return false };
    let Some(procs) = v.pointer("/result/process_info/foreground_processes").and_then(|p| p.as_array()) else {
        return false;
    };
    !procs.is_empty()
        && procs.iter().all(|p| {
            let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("");
            matches!(name.trim_start_matches('-'), "zsh" | "bash" | "fish" | "sh" | "dash" | "ksh" | "nu" | "tcsh" | "elvish" | "xonsh")
        })
}

/// Give the popup a label + identity token.
pub fn announce_pane(label: &str) {
    let Ok(id) = std::env::var("HERDR_PANE_ID") else { return };
    if !flag_safe(&id) {
        return;
    }
    let _ = herdr(&["pane", "rename", &id, label]);
    let _ = herdr(&["pane", "report-metadata", &id, "--source", PLUGIN_ID, "--token", &format!("{PLUGIN_ID}=1")]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_pane_ids() {
        let split = r#"{"id":"x","result":{"pane":{"pane_id":"w1:p9","tab_id":"w1:t1"},"type":"pane_info"}}"#;
        assert_eq!(pane_id_in(split).as_deref(), Some("w1:p9"));
        let tab = r#"{"result":{"root_pane":{"pane_id":"w1:p2"},"tab":{"tab_id":"w1:t3"}}}"#;
        assert_eq!(pane_id_in(tab).as_deref(), Some("w1:p2"));
        assert_eq!(pane_id_in(r#"{"result":{"pane":{"pane_id":"--evil"}}}"#), None);
    }
}
