# `herdr-oil` Next-Wave Feature Requests & Improvement Suggestions

This document outlines the next wave of suggested enhancements for `herdr-oil` (building on the completed feature wave now reflected in the README), organized across **added functionality**, **visualization & UI polish**, **ease of use & performance**, and **git integration**.

## Status Notes

These items from earlier review passes are already implemented and should not be re-added as open work:

- **Line-aware marked grep hits:** `Finder::marked` now stores `{ path, line }`, so `^space` + `^s` preserves `:line` for grep results.
- **Stale undo journal after non-undoable applies:** `App::execute` now clears/replaces `last_batch` when a landed apply has no undo ops, such as permanent deletes.
- **Searchable help:** `g?` opens help and `/` filters it.

The highest-impact remaining items are the Git polish items in section 4, especially defaulting the Git finder to the repository root, deleted-file diff viewing, and making Git status richer and faster.

---

## 1. Added Functionality

### A. Buffer & Vim Power-Editing
1. **Sequential Numbering / Counter Token in `:%s` and `:prefix` / `:suffix`**
   - **Current state:** `substitute` (`src/vim.rs`) and `Vim::ex_range` (`src/vim.rs`) support regex capture groups (`$1` / `\1`), static prefixes, and static suffixes.
   - **Suggestion:** Support a counter token such as `{#}` / `{03#}` (or `\=i`) across a range (e.g., `:'<,'>s/^/{02#}_/` or `:%s/IMG_\d+/photo_{03#}/`). Sequential renumbering of screenshots, migrations, or test fixtures is one of the most common batch-rename tasks that plain regex captures cannot express.
2. **`:g/pat/cmd` & `:v/pat/cmd` (Global Filter Operations)**
   - **Current state:** Range commands in `Vim::ex_range` (`src/vim.rs`) operate on contiguous line ranges (`%`, `'<,'>`, `n,m`).
   - **Suggestion:** Add `:g/pattern/d` (delete all matching lines, e.g. `:g/\.bak$/d` or `:g/\.DS_Store/d`), `:v/pattern/d` (keep only matching lines), and `:g/pattern/s/.../.../` in `Vim::ex` (`src/vim.rs`). This pairs naturally with the live preview infrastructure (`sub_base`) already in `Vim::live_substitute`.
3. **Preserve Source Entry Lookups on Edited Lines (`Buffer::cur_source_entry`)**
   - **Current state:** `Buffer::cur_entry` (`src/buffer.rs`) still returns `None` once a line's source name no longer matches the entry name. As a result, while a line is renamed in the buffer, its preview disappears and commands that expect an existing source entry can fall back to the current directory.
   - **Suggestion:** Add a helper `Buffer::cur_source_entry` that looks up `l.id` in `Store` (`src/entry.rs`) regardless of whether `l.name()` was edited, so previews, `gy`, `gi`, and `:chmod` continue to work on lines with pending renames.

### B. Finder & Grep Workflow
4. **Live Grep / Finder → Editable Buffer ("Quickfix / Oil-Grep" Mode)**
   - **Current state:** In `Finder` (`src/finder.rs`), `^space` marks multiple files or grep hits, marks keep line numbers, `^s` sends them, and `^o` reveals a single file in the explorer.
   - **Suggestion:** Add a binding in Finder (e.g., `^q` or `alt-e`) that loads the unique files from the current Finder/Grep results (or `marked` files) into a virtual multi-directory Oil buffer where each line displays `rel/path/to/file.rs`. Because `Store` (`src/entry.rs`) and `plan::build` (`src/plan.rs`) already track full `PathBuf`s per entry ID, users can batch-rename, move, or delete files across the entire repository from a fuzzy query or ripgrep search.
5. **Archive Extraction / Creation (`:extract` / `:archive`)**
   - **Current state:** `preview::special` (`src/preview.rs`) previews `.zip` and `.tar.*` contents.
   - **Suggestion:** Support `:extract` on an archive entry to unpack it into a subdirectory, and `:zip name.zip` / `:tar name.tar.gz` on a visual selection (`'<,'>`) to pack selected entries.

---

## 2. Visualization & UI Polish

### A. Explorer & Buffer Rendering
1. **Highlight Search Matches (`/`) Inside Explorer & Live `:s` Spans**
   - **Current state:** The built-in file viewer highlights `/` search matches using `mark_matches` (`src/ui.rs`), and the confirm dialog highlights character diffs using `diff_spans` (`src/ui.rs`). However, in `draw_explorer` (`src/ui.rs`), typing `/pattern` only moves the cursor to the matching line without highlighting the matched substring, and typing `:%s/pat/rep/` replaces the line text in-place without highlighting the replaced span.
   - **Suggestion:** Reuse `mark_matches` in `draw_explorer` when `vim.mode` is `Cmd { kind: '/', .. }` (or when `vim.search` is active), and highlight the substituted spans in `th.yellow` / `th.green` during `Vim::live_substitute` (`src/vim.rs`).
2. **Inline Conflict & Error Highlighting Before `:w`**
   - **Current state:** `draw_explorer` (`src/ui.rs`) runs `plan::build` when buffers are modified and shows `⚠ N problem(s)` in the header, and marks syntax-invalid lines (`parse_name` error) with a red `!`. However, if two lines collide (`two entries would get this name`) or a rename clashes with an existing file on disk (`already exists`), those lines still show `~` or `+` until `:w` opens the error modal.
   - **Suggestion:** Have `plan::validate` (`src/plan.rs`) record a `HashSet<String>` of conflicting destination keys (`plan::key(dst)`), so `draw_explorer` can immediately mark colliding lines with a red `!` and surface the conflict reason in the status bar while editing.
3. **Breadcrumb / Parent Context or Miller-Column Option**
   - **Current state:** `draw_explorer` (`src/ui.rs`) splits the body 48% / 52% between the current directory and the preview pane.
   - **Suggestion:** In wide popups (`width >= 90`), offer an optional narrow parent-directory column on the left (15% / 40% / 45%, toggled via `gw` or config) or interactive clickable path segments in the top header bar (`head` in `draw_explorer`) so mouse users can click any ancestor directory in `~/Projects/herdr-oil/src/` to jump straight to it.

### B. Preview & Viewer Enhancements
4. **PDF Metadata / Text Previews**
   - **Current state:** `preview::special` (`src/preview.rs`) handles archives, SQLite schemas, binary plists, image metadata, and generic binary summaries, but PDFs still fall through to the generic binary path.
   - **Suggestion:** Support `.pdf` metadata/text preview via `pdftotext`, `pdfinfo`, or `mdls` (macOS) in `preview::special`.

---

## 3. Ease of Use & Performance

### A. Ergonomics & Workflow
1. **Command & Search History (`Up` / `Down` in `:` and `/` Modes)**
   - **Current state:** In `Vim::cmd_key` (`src/vim.rs`), pressing `KeyCode::Up` or `KeyCode::Down` while typing a `:` command or `/` search is ignored.
   - **Suggestion:** Store a `Vec<String>` history for `:` commands and `/` searches in `Vim` (`src/vim.rs`, optionally persisted in `state_dir`), allowing `Up` / `Down` (and `^p` / `^n`) to recall previous substitutions like `:%s/.../.../` or `:chmod` commands.
2. **Unified Line-Editing Keys Across `:` Command Mode, `/` Search, and Viewer**
   - **Current state:** `App::finder_key` (`src/app.rs`) supports `^w` (delete word back), `^a` / `Home`, and `Alt-e` / `End`. By contrast, `Vim::cmd_key` (`src/vim.rs`) and `App::viewer_key` (`src/app.rs`) only handle `Backspace` and `^u`.
   - **Suggestion:** Extract a small shared line-input helper so `^w`, `^a`, `^e`, `Home`, `End`, and `Left` / `Right` cursor movement work identically across Finder, `:` command mode, `/` search, and Viewer `/` search.
3. **Interactive Confirm Modal (Jump to Entry / Drop Single Op)**
   - **Current state:** When edits span multiple directories, the header shows `(N other dirs)`, and `:w` opens `Overlay::Confirm` (`src/app.rs`), which lists operations with `j` / `k` scroll but does not allow acting on individual items.
   - **Suggestion:** Add a cursor selection inside `Overlay::Confirm` so pressing `gd` / `⏎` jumps directly to that directory/line in the Explorer, or pressing `x` / `d` drops that single operation from the plan before applying.
4. **Expanded Tab Completion (`:chmod`, `:sort`, and Buffer Filenames)**
   - **Current state:** `Vim::complete` (`src/vim.rs`) completes ex command names and paths after `:cd` / `:e`.
   - **Suggestion:** Extend `Vim::complete` to complete `:sort ` arguments (`name`, `mtime`, `size`, `ext`), `:chmod ` presets (`+x`, `-x`, `644`, `755`), and current-buffer filenames when typing `:s/` or `:touch `.

### B. Performance & Responsiveness
5. **Background Preview Rendering**
   - **Current state:** `Cache::get` (`src/preview.rs`) runs `bat`, `git diff`, `tar`, or `unzip` synchronously inside `ui::draw` while holding the terminal frame. Holding down `j` / `k` in the Explorer or Finder spawns a subprocess on every key repeat before the frame can draw.
   - **Suggestion:** Debounce or run `preview::render` (`src/preview.rs`) on a background thread so `j` / `k` cursor movement stays 60fps even when `bat` or `git diff` takes 30–50ms.
6. **Cache `plan::build` Across Frames in `draw_explorer`**
   - **Current state:** Whenever any buffer is modified, `draw_explorer` (`src/ui.rs`) runs `plan::build` (`src/plan.rs`)—including `std::fs::symlink_metadata(dst)` inside `plan::validate`—on **every UI draw tick** (including spinner ticks and cursor movements).
   - **Suggestion:** Cache the computed `Plan` on `App`, keyed by the sum of `Buffer::edits` across `app.buffers` (and `live_substitute` state), so `plan::build` and its `symlink_metadata` syscalls only run when buffer text actually changes.

---

## 4. Git Integration Improvements

### A. Bug Fixes & Edge Cases
1. **Enable Rename Detection in `git status` (`R` Status)**
   - **Current state:** `status_records` (`src/entry.rs`) passes `--no-renames` to `git status`, even though lines 441–443 and `status_letter` explicitly parse `R` (renamed) and `C` (copied) records. As a result, `git mv` shows up as separate `A` and `D` entries instead of `R`.
   - **Suggestion:** Remove `--no-renames` (or pass `--find-renames`) so staged renames properly appear as `R`.
2. **Open Deleted (`D`) Files in Diff Mode in the Built-in Viewer**
   - **Current state:** In the Finder's `Git` tab (`draw_finder` in `src/ui.rs`), the side preview passes `diff: true` so deleted files render `git diff HEAD`. However, pressing `⏎` opens `Viewer` (`draw_viewer` in `src/ui.rs`) with `diff: false`, which calls `std::fs::metadata(&v.path)` in `preview::render` and fails with `No such file or directory`.
   - **Suggestion:** When opening a file from the `Git` tab (or when the file is deleted on disk but tracked in git), open `Viewer` in diff mode (and allow toggling diff mode in `Viewer` with `d`).
3. **Handle Unborn Branches (`HEAD` Missing After `git init`)**
   - **Current state:** `preview::git_diff` (`src/preview.rs`) runs `git diff ... HEAD -- <path>`. In a newly initialized repository before the first commit, `HEAD` does not exist and `git diff HEAD` exits with an error, causing the preview to fall back to raw file contents even for staged files.
   - **Suggestion:** Fall back to `git diff --cached -- <path>` (or `git diff -- <path>`) when `HEAD` cannot be resolved.
4. **Default the Finder `Git` Tab to the Repository Root**
   - **Current state:** Opening the `Git` finder tab from a subdirectory (`␣s`) runs `entry::git_changes(&self.root)` where `self.root` is `app.cwd`, hiding modified files in sibling directories unless the user manually presses `^r`.
   - **Suggestion:** Default `FinderKind::Git` to `finder::repo_root(&self.cwd).unwrap_or_else(|| self.cwd.clone())` (matching how `start_transfer` already initializes `gm` / `gc`).
   - **Priority:** High. This is small, testable, and makes the Git tab match user expectations immediately.

### B. Richer Git Visualization
5. **Distinguish Staged vs. Unstaged vs. Partially Staged (`XY` Status)**
   - **Current state:** `status_letter` (`src/entry.rs`) collapses the two-character porcelain `XY` code into a single `char` (`M`, `A`, `D`, `?`, `!`). Users cannot tell whether changes are staged (`M `), unstaged (` M`), or both (`MM`).
   - **Suggestion:** Preserve both `X` (index) and `Y` (worktree) state so `draw_explorer` and `draw_finder` can color-code or render two-column markers:
     - **Green (`M`, `A`, `D`, `R`):** Staged in index only
     - **Yellow `M` / Red `D`:** Unstaged worktree changes
     - **Peach `M` (or `±`):** Both staged and unstaged changes (`MM`, `AM`)
6. **Branch Name & Ahead/Behind Badge in Explorer Header**
   - **Current state:** The Explorer header shows only the directory path, pending buffer edits, and sort mode.
   - **Suggestion:** Pass `-b` to `git status --porcelain=v1 -b -z` to parse the `## branch...upstream [ahead N, behind M]` header at zero extra subprocess cost, and display a compact branch badge (e.g. ` main ↑1`) in `draw_explorer`.
7. **Diff Toggle (`gD` / `d`) & Inline Diff Gutter Indicators**
   - **Current state:** `draw_preview` (`src/ui.rs`) always passes `diff = false` in the Explorer view (`diff = true` is only used in the Finder's Git tab).
   - **Suggestion:** Add `gD` in the Explorer (and `d` in the Viewer) to toggle between file content and `git diff HEAD` (or staged vs. unstaged diff), and optionally render subtle `│` gutter bars (green added, yellow modified, red deleted) next to line numbers in normal file previews.
8. **Keep Git Status Badge Visible While Renaming a Line**
   - **Current state:** In `draw_explorer` (`src/ui.rs`), `entry.filter(|e| e.name == name_now).and_then(|e| b.git.get(&e.name))` hides a file's git status marker as soon as its text is edited in the buffer.
   - **Suggestion:** Look up `b.git.get(&e.name)` via the entry's original name even while the buffer line is being edited.

### C. Git Workflow & Navigation Actions
9. **Jump Between Modified Entries (`]c` / `[c` or `]g` / `[g`)**
   - **In Explorer:** Jump the cursor directly to the next/previous entry (file or folder) with a non-clean git status (`M`, `A`, `?`, `U`, `D`).
   - **In Viewer:** Jump between diff hunks when viewing a file or diff.
10. **Stage, Unstage, and Discard Changes (`:stage`, `:unstage`, `:restore`, Finder `^a`)**
    - **In the Finder `Git` tab (`␣s`):** Bind `^a` (or `gs`) to toggle staging on the selected/marked file(s) (`git add --` / `git restore --staged --`) and refresh the list and diff preview in-place; bind `alt-d` to discard unstaged worktree changes after a confirmation prompt.
    - **In the Explorer:** Support `:stage`, `:unstage`, and `:restore` on the cursor line or visual selection (`:'<,'>stage`).
11. **Filter Explorer to Changed Entries Only (`:git-filter` or `gG`)**
    - **Suggestion:** Add a toggle in the Explorer that filters the current directory buffer to only entries with active git status markers (including directories that contain dirty children), making it effortless to focus on modified files in large directories.
12. **File / Directory Commit Log Preview (`gl`)**
    - **Suggestion:** Pressing `gl` on an entry in the Explorer or Viewer opens `git log --oneline --graph --decorate -n 50 -- <path>` in the Viewer so users can inspect recent commits touching that file or folder.

### D. Git Subprocess Performance
13. **Consolidate the 3 Git Subprocesses into 1 Cached Call & Run Asynchronously**
    - **Current state:** `status_records` (`src/entry.rs`) spawns **three** synchronous `git` processes every time a directory is entered or refreshed:
      1. `git config --list --show-scope --includes --name-only` (in `repo_defines_filters`)
      2. `git status --porcelain=v1 -z ...`
      3. `git rev-parse --show-prefix`
    - **Suggestion:**
      - Cache `repo_defines_filters` and the repository root (`git rev-parse --show-toplevel`) per repo in `App`, and compute the relative prefix in pure Rust (`dir.strip_prefix(&repo_root)`), reducing 3 subprocesses to **1**.
      - Run that single `git status` call on a background worker thread, updating `Buffer::git` asynchronously so directory navigation is instant even in large repositories.

---

## 5. Additional Ambitious Ideas

### A. Safety, Review & Undo
1. **Trash / Apply History Browser (`:history` or `:trash`)**
   - **Suggestion:** Add an overlay that lists recent `:w` batches, deleted items moved to Trash, undoability, permanent-delete gaps, and restore actions.
   - **Why:** `U` is powerful but invisible. A history browser would make destructive operations feel inspectable and recoverable.

2. **Conflict-Fix Actions in the Error / Confirm Flow**
   - **Suggestion:** When `:w` reports collisions, offer single-key fixes: append a counter, reveal the conflicting entry, drop one operation, or jump to the source line.
   - **Why:** The current safety model prevents damage, but the user still has to manually navigate back and repair the buffer.

3. **Safe Dry-Run Export**
   - **Suggestion:** Add `:plan-copy` or `:plan > path` to copy/export the pending plan as readable text.
   - **Why:** Useful for code reviews, agent handoff, audit trails, and “show me what this will do” workflows.

### B. Rename & Batch Operation Power
4. **Named Operation Recipes**
   - **Suggestion:** Support reusable rename recipes such as `:recipe slugify`, `:recipe screenshots`, `:recipe kebab`, or user-defined recipes in config.
   - **Why:** Many batch rename patterns repeat across projects and should not require retyping complex regexes.

5. **Counter-Aware Conflict Resolution**
   - **Suggestion:** Pair the counter token feature with automatic collision repair, e.g. `report_{03#}.pdf` or “append counters to all conflicting destinations.”
   - **Why:** Counter renaming and collision repair solve the same real-world batch-renaming pain.

### C. Richer Explorer Modes
6. **File Metadata Mode**
   - **Suggestion:** Add a toggle for owner/group, xattrs, executable bit, symlink health, broken links, and extended file attributes.
   - **Why:** `details` is already useful; a deeper inspection mode would turn herdr-oil into a serious maintenance tool.

7. **Project Dashboard Finder Tab**
   - **Suggestion:** Add a tab that combines Git changes, recent files, marked paths, and pending Oil edits into one triage view.
   - **Why:** The plugin now has several powerful modes. A dashboard would make it feel like a workspace cockpit rather than separate tools.

8. **Context Send to Agents**
   - **Suggestion:** Extend `send` beyond paths: send selected grep lines, nearby context, preview snippets, or Git diff hunks to the origin pane.
   - **Why:** Herdr users often work with coding agents; sending exactly the useful context is more valuable than sending only filenames.

### D. Recommended Implementation Order
1. Default the Git finder to the repository root.
2. Open deleted Git files in diff mode and add a viewer diff toggle.
3. Keep Git status visible while renaming.
4. Add command/search history and unified line-editing keys.
5. Move preview rendering off the draw path.
6. Add the history/trash browser once the Git and preview experience feels solid.
