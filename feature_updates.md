# `herdr-oil` Feature & Improvement Suggestions

This document outlines suggested enhancements for `herdr-oil` across **added functionality**, **visualization & UI polish**, and **ease of use & performance**.

> **Interactive UI Mockup:** Open [`docs/feature_updates_mockup.html`](docs/feature_updates_mockup.html) in a browser to review interactive Before/After TUI screens, a live `:%s` & symlink/chmod buffer sandbox, and a filterable roadmap matrix for all suggestions below.

---

## 1. Added Functionality

### A. Bulk Editing & Power-User Vim Features
Because the central premise of `herdr-oil` is *"the directory listing is a text buffer"*, users naturally reach for bulk-refactoring idioms when renaming dozens of files:
- **`:s/pat/rep/[g]` & `:%s/...` Substitution**: Implement `:s` (current line or visual selection `:'<,'>s`) and `:%s` (whole buffer) in `src/vim.rs` (`Vim::ex`). This unlocks instant batch renaming (e.g., `:%s/\.jpeg$/.jpg/` or `:%s/^draft-/final-/`).
- **Dot Repeat (`.`) & `,` (Reverse char-find)**: Repeating the last normal-mode edit (`cw`, `cin`, `x`, `s`, `~`) across lines via `.` is the most common reflex after `cin` when renaming several files. Pairing `;` with `,` in `Vim::motion` completes `f`/`t` navigation.
- **Edit Directory in `$EDITOR` (`:edit-buffer` or `ge`)**: For complex multi-cursor or macro renames, allow suspending the TUI and opening the current buffer lines in `$EDITOR` as a temporary file (keeping hidden entry IDs mapped by line or `/001` prefix like `oil.nvim`), then parsing it back into `Buffer` on exit.
- **Visual Block / Multi-Line Prefix & Suffix**: Since `^v` is bound to split-right, adding an ex command like `:prefix str` / `:suffix str` or visual `I` / `A` (prepend/append to all selected lines) makes batch prefixing effortless.

### B. File System & Permission Operations
- **Symlink Creation & Editing**: Currently `Entry::link_target` in `src/entry.rs` is rendered as virtual tail text (` → target`), so it is not editable. Supporting syntax like `link_name -> target_path` in `plan::parse_name` (or an `:ln src dst` command) would let users create and retarget symlinks directly in the buffer.
- **Executable Bit / `chmod` (`:chmod +x` or mode editing)**: When creating scripts with `o` (e.g., `deploy.sh`), users frequently need `chmod +x`. Supporting `:chmod +x` (or `:chmod 755`) on the current/visual lines—staged in `Op` or applied directly—avoids dropping to a shell.
- **Post-Apply Undo (Transaction Rollback)**: `Buffer::undo` works prior to `:w`, but once `apply::apply` runs, `App::reload_all` clears all history. Because `apply` is already cleanly modeled as reversible `Op`s (Moves, Copies, Creates, and Trash paths returned by `apply::trash`), storing the last applied `Report` would allow `:undo-apply` (`U`) to reverse the last disk transaction.

### C. Richer Finder & `herdr` Integration
- **5th Finder Mode — `Git` (`␣s` / Modified files)**: Add a `FinderKind::Git` tab to `Finder` (`src/finder.rs`) that lists `git status` modified/untracked files across the repository and shows `git diff` in the preview pane for fast workspace review.
- **Ripgrep Glob/Type Filtering in Grep Mode**: In `Finder::launch_grep` (`src/finder.rs`), the entire query is passed after `--`. Supporting a separator (e.g., `foo -- -t rust` or `foo :: *.rs`) to pass file globs/types to `rg` would drastically improve live grep in large repositories.
- **Send Path(s) to Origin / Agent Pane (`gp` or `^i`)**: In `open::open` (`src/open.rs`), `herdr-oil` rightly avoids typing `$EDITOR file` into an agent pane. However, when working with AI coding agents in `herdr`, a dedicated key (`gp` or `^i` for "paste path into origin pane") that sends the relative/absolute path of the selected or marked files (e.g. `src/app.rs:42`) into the origin pane is a major workflow booster.

---

## 2. Visualization & UI Polish

### A. Inline Diffs in Explorer & Confirm Dialog
- **Show Original Name When Renaming Inline**: In `ui::draw_explorer` (`src/ui.rs`), when a user edits `report.md` to `final.md`, the line shows `~ final.md`, and the original name disappears. Rendering the original name dimmed in the virtual tail (e.g., `~ final.md  ← report.md`) makes it immediately clear *which* entry is being renamed without having to press `:w` or `u`.
- **Pending Deletions Indicator in Current Dir**: When lines are deleted (`dd`), the header shows `−2`, but there is no visual cue in the buffer list itself of *what* was deleted in the current directory. A bottom virtual footer (`── 2 deleted: old.txt, tmp/ ──`) or a `:diff` toggle would help prevent accidental deletions.
- **Character-Level Diff Highlighting in Confirm Modal**: In `ui::draw_confirm` (`src/ui.rs`), `RENAME  very_long_filename_v1.rs → very_long_filename_v2.rs` is rendered in flat `th.text`. Highlighting the changed spans (`v1` vs `v2`) makes reviewing batch renames much faster.

### B. Richer Previews (`src/preview.rs`)
- **Directory Tree / README Summary Preview**: Currently `preview::dir_listing` shows a flat 1-level list without sizes or git markers. Enhancing directory previews to show item/size stats, git status markers, or a rendered `README.md` snippet at the bottom adds useful context.
- **Structured Binary & Archive Previews**: In `preview::render`, any file with a null byte only runs `file -b` on macOS. This can be extended to:
  - **Archives (`.zip`, `.tar.gz`, `.tgz`, `.whl`)**: List archive contents via `tar -ztvf` / `unzip -l`.
  - **Images / Media / PDFs**: Show dimensions/metadata via `sips -g all` (macOS) or `file -b` (enabled on Linux as well), or optional terminal graphics protocols (Kitty/iTerm2/Sixel).
  - **SQLite / `.plist`**: Run `sqlite3 ... .schema` or `plutil -p`.

### C. Unicode Width & Column Alignment
- **Multi-byte / CJK Cursor & Truncation Alignment**: In `ui::draw_explorer` (`src/ui.rs`), horizontal scrolling (`avail`), `shown`, and `cursor_pos` use `.chars().count()` rather than display width (`unicode_width::UnicodeWidthChar`). If a filename contains CJK characters or wide emojis, the cursor position and right-aligned detail column (`tail_w`) will drift by 1 cell per wide character.

---

## 3. Ease of Use & Performance

### A. Navigation & History
- **Directory Jump History (`H` / `L` or `^o` / `^i`)**: Users can go up (`-`), home (`~`), or to start (`_`), but there is no back/forward history stack when drilling down or jumping via Finder. Maintaining a `Vec<PathBuf>` navigation history in `App` (`src/app.rs`) so `H` / `L` traverses visited directories is a big ergonomic win.
- **Bookmarks / Marks (`m{a-z}` and `'{a-z}`)**: Standard vim marks (`ma` to mark the current directory in `state_dir/marks.toml`, `'a` to jump to it across sessions) fit naturally into the `oil.nvim` mental model.
- **Tab Completion in `:` Command Mode**: Typing `:cd src/co<Tab>` or `:e <Tab>` in `Vim::cmd_key` (`src/vim.rs`) currently inserts nothing. Adding path completion on `KeyCode::Tab` when `kind == ':'` makes `:cd` and `:e` much smoother.
- **Search (`/`) Inside the Built-in Viewer**: When `open = "view"` (the default), `App::viewer_key` (`src/app.rs`) supports scrolling (`j/k/d/u/g/G`), but not `/` search or `n`/`N` next-match jumping inside the viewed file.

### B. Yank & Clipboard Variants
- **`gy` vs `gY` / `gn`**: Currently `gy` yanks the full absolute path (`Action::YankPath`). Providing granular yank bindings saves manual trimming:
  - `gy`: relative path from repository root / start directory
  - `gY`: absolute path
  - `gn`: filename only (`Entry::name`)

### C. Performance & Responsiveness on Large Repositories
- **Cap or Background Directory Child Counts**: In `entry::stat_entry` (`src/entry.rs`), every directory child listed runs `fs::read_dir(path).ok().map(|rd| rd.take(100_000).count())` synchronously on the UI thread. If a directory contains a `node_modules/` or `target/` subdirectory with 50k+ items, entering the parent directory can stall while counting children. Capping `.take(1_000)` (`"1k+ items"`) or computing directory counts + `git status` asynchronously on a background thread will keep directory navigation instant.
- **Sort Direction Toggle (`gs` vs `gS`)**: `Sort::next` (`src/entry.rs`) cycles `Name → Mtime → Size → Ext`, always descending for `Mtime`/`Size` and ascending for `Name`/`Ext`. Adding `gS` (or `:sort!`) to reverse sort order is useful when looking for oldest or smallest files.
