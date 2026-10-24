//! herdr-oil: an oil.nvim-style file explorer and fuzzy finder for herdr.
//!
//!   herdr-oil                    run the TUI (dir from $HERDR_OIL_ROOT, else cwd)
//!   herdr-oil --launch MODE      open the popup for the focused pane (explorer|finder|files|grep|recent|git|dash|projects)
//!   herdr-oil [DIR|FILE]         explore DIR (or FILE's directory, cursor on it)
//!   herdr-oil --finder|--grep    start in the finder
//!   herdr-oil --pick             print the chosen file(s) to stdout instead of opening

mod app;
mod archive;
mod apply;
mod buffer;
mod config;
mod diff;
mod diffview;
mod entry;
mod finder;
mod git;
mod input;
mod launch;
mod open;
mod plan;
mod preview;
mod projects;
mod theme;
mod ui;
mod vim;

use crossterm::event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture};
use crossterm::execute;
use std::path::PathBuf;

fn usage() {
    println!(
        "herdr-oil {}\n\nusage: herdr-oil [--launch MODE] [--finder|--files|--grep|--recent|--git|--dash|--projects] [--pick] [DIR|FILE]\n\n\
         MODE: explorer (default), finder, files, grep, recent, git, dash, projects",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut mode = std::env::var("HERDR_OIL_MODE").unwrap_or_else(|_| "explorer".into());
    let mut start: Option<PathBuf> = None;
    let mut pick = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--launch" => {
                let m = args.get(i + 1).cloned().unwrap_or_else(|| "explorer".into());
                if let Err(e) = launch::run(&m) {
                    eprintln!("herdr-oil: {e}");
                    std::process::exit(1);
                }
                return;
            }
            "--finder" => mode = "finder".into(),
            "--files" => mode = "files".into(),
            "--dash" => mode = "dash".into(),
            "--grep" => mode = "grep".into(),
            "--recent" => mode = "recent".into(),
            "--git" => mode = "git".into(),
            "--projects" => mode = "projects".into(),
            "--pick" => pick = true,
            "--version" | "-V" => {
                println!("herdr-oil {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "-h" | "--help" => {
                usage();
                return;
            }
            s if s.starts_with('-') => {
                eprintln!("herdr-oil: unknown argument {s}");
                std::process::exit(2);
            }
            s => start = Some(PathBuf::from(s)),
        }
        i += 1;
    }
    let start = start
        .or_else(|| std::env::var("HERDR_OIL_ROOT").ok().filter(|s| !s.is_empty()).map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"));
    let start = if start.exists() { start } else { std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")) };

    // Only label panes we were launched into (the plugin popup), never a
    // user's own pane where they ran `herdr-oil` by hand.
    if !pick && std::env::var("HERDR_OIL_ROOT").is_ok() {
        launch::announce_pane(match mode.as_str() {
            "explorer" => "Oil",
            "projects" => "Projects",
            _ => "Find",
        });
    }
    let cfg = config::Config::load();
    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    let mut app = app::App::new(start, &mode, cfg, pick);
    let result = app.run(&mut terminal);
    let _ = execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    if let Err(e) = result {
        eprintln!("herdr-oil: {e}");
        std::process::exit(1);
    }
    for p in &app.picked {
        println!("{}", p.display());
    }
}
