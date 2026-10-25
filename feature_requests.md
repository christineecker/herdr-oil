# `herdr-oil` Feature Requests

Open requests only. Implemented items are removed from this file once they land; the README describes what exists.

Items marked **Review note** were checked against the code; the note records corrections to the "current state" or the design decision needed before implementing.

---

## 1. Finder

1. **Live Grep / Finder → Editable Buffer ("Quickfix / Oil-Grep" Mode)** — ⏸ deferred
   - **Current state:** In `Finder` (`src/finder.rs`), `^space` marks multiple files or grep hits, marks keep line numbers, `^s` sends them, and `^o` reveals a single file in the explorer.
   - **Suggestion:** Add a binding in Finder (e.g., `^q` or `alt-e`) that loads the unique files from the current Finder/Grep results (or `marked` files) into a virtual multi-directory Oil buffer where each line displays `rel/path/to/file.rs`. Because `Store` (`src/entry.rs`) and `plan::build` (`src/plan.rs`) already track full `PathBuf`s per entry ID, users can batch-rename, move, or delete files across the entire repository from a fuzzy query or ripgrep search.
   - **Review note:** Large. `Buffer` is built around one directory (`b.dir`): line text is a name relative to it, and `plan::build` joins it to `b.dir`. A multi-directory buffer needs a new line/path model throughout `buffer.rs`, `plan.rs`, and `draw_explorer`. Defer.

2. **Projects tab: features not yet ported from herdr-hop**
   - Templates and `gh repo create`, the GitHub browser and clone, worktree create, start agent, lazygit split, zoxide ranking, `[recipe]`.
