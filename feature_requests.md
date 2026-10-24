# `herdr-oil` Next-Wave Feature Requests & Improvement Suggestions

This document outlines the next wave of suggested enhancements for `herdr-oil` (building on the completed feature wave now reflected in the README), organized across **added functionality**, **visualization & UI polish**, **ease of use & performance**, and **git integration**.

## Status Notes

These items from earlier review passes are already implemented and should not be re-added as open work:

- **Line-aware marked grep hits:** `Finder::marked` now stores `{ path, line }`, so `^space` + `^s` preserves `:line` for grep results.
- **Stale undo journal after non-undoable applies:** `App::execute` now clears/replaces `last_batch` when a landed apply has no undo ops, such as permanent deletes.
- **Searchable help:** `g?` opens help and `/` filters it.
- **Git tab at repository root:** `FinderKind::Git` (opened directly or via `⇥`) roots at `finder::repo_root`; leaving the tab restores the previous root (`Finder::pre_git`).
- **Viewer diff mode:** `Viewer::diff` starts on from the Git tab and for deleted files; `d` toggles it.
- **Git badge while renaming:** kept on lines whose plan tag is a rename (`~`), not on copies or moved-in lines.
- **Inline conflicts:** `Plan::conflicts` (dst key → reason) marks clashing lines with a red `!` and `✗ reason` before `:w`.
- **Unborn branch diffs:** `preview::git_diff` falls back to `git diff --cached` when `HEAD` does not resolve.
- **`:` / `/` history:** `Up` / `Down` / `^p` / `^n` with prefix filtering, persisted in `state_dir/history.txt`.

## Implementation Status (2026-10-24)

Everything below is implemented except **1.B.4 (multi-directory "Oil-Grep" buffer)**, which stays deferred as its review note recommends (it needs a new line/path model across `buffer.rs`, `plan.rs` and `draw_explorer`). Items are marked ✅. Decisions taken where the review notes left a choice:

- **Counter token (1.A.1):** `{#}` → 1, 2, 3; `{03#}` zero-pads to width 3; `{#10}` starts at 10; `{#0+5}` steps by 5. The counter advances **per match** (so `g` with several matches per line counts each), in buffer order across the range, and runs on across the lines a `:g/…/s` touches. `:prefix` / `:suffix` count per line. `\{` writes a literal brace.
- **`:g` / `:v` (1.A.2):** commands `d` and `s/…/…/` only. The live preview does not remove lines: the doomed ones are struck through in red with a `−` marker and the status shows "N lines to delete". Deleted lines go to the register as a cut, so `p` elsewhere turns them into moves. Default range is the whole buffer.
- **Source entry (1.A.3):** new `Buffer::cur_source_entry`; used by the explorer preview and `gy` / `gY` / `gn`. `cur_entry` is unchanged.
- **Archives (1.B.5):** `:extract` lists members first and refuses absolute paths, `..`, symlinks and hard links; it always extracts into a new folder (`name`, then `name 2`, …). `:zip` / `:tar` never overwrite; `:tar` picks compression from the extension (`.tar.gz` default).
- **Explorer highlights (2.A.1):** `/` matches stay highlighted until `:noh`; a live `:s` underlines the changed span in green.
- **Parent column (2.A.2):** `gw` toggles a 15% parent column at ≥ 90 columns; header path segments are clickable.
- **PDF (2.B.3):** `pdfinfo` + first three pages via `pdftotext`, else `mdls` on macOS.
- **Line editing (3.A.1):** `src/input.rs`, shared by the finder, `:`, `/` and the viewer prompt: `^a`/`Home`, `^e`/`alt-e`/`End`, `alt-b`/`alt-f`, `^w`/`alt-⌫`, `^u`, `^k`, `Left`/`Right`, `Delete`.
- **Confirm modal (3.A.2):** `j`/`k` pick an op, `x`/`d` drop it (the plan is re-validated; apply is refused while problems remain), `o` jumps to the line. `⏎` still applies, so `o` (not `⏎`) is the jump key.
- **Completion (3.A.3):** `:sort`, `:chmod`, `:recipe` arguments; listed names after `:s/`, `:touch`, `:mkdir`; ranges are kept (`%recipe sl⇥`).
- **Background previews (3.B.4):** `preview::Cache` renders on a worker thread with a newest-request-wins queue; the loop polls every 16 ms while something is pending; the viewer shows "loading…" until its rendering lands.
- **Plan cache (3.B.5):** keyed by a hash of every modified buffer's lines (ids and text), not by edit counts, and dropped on navigation and reloads.
- **Renames (4.1):** `status.renames=true`; the folder a file was renamed out of is marked too.
- **XY status (4.2):** `entry::Git { letter, staged, unstaged }`; folders merge children (strongest letter, staged/unstaged OR-ed). Colours: green staged only, yellow (red for `D`) unstaged only, peach both.
- **Branch badge (4.3):** from `git status -b` (same call); handles unborn and detached `HEAD`; absent where filters are defined.
- **`gD` / gutter (4.4):** `gD` opens the diff view for the cursor entry (a folder: all its changes). The viewer gutter marks lines added (`│` green), edited (`│` yellow) and removals (`▁` red) against `HEAD`.
- **Jumps (4.5):** `]g` `[g` and `]c` `[c` in the explorer (ignored entries skipped, wraps); `]c` `[c` jump hunks in the diff view.
- **Staging (4.6):** Git tab `^a` toggles (fully staged → unstage, otherwise stage), `alt-d` discards after a yes/no dialog; explorer `:stage`, `:unstage`, `:restore` on ranges. `git add` / `git restore` run only behind the filter guard with hooks and fsmonitor off; before the first commit `:unstage` uses `git rm --cached`.
- **Changed-only (4.7):** `gG` (or `:changed`) reloads the listing with only entries that have a git status; `Buffer::original` is filtered too, so hidden entries are never planned as deletes. Refused while the buffer has edits.
- **Log (4.8):** `gl` in the explorer; in the viewer `L` (single `g` already means "top" there).
- **Git subprocesses (4.9):** the work tree is found in Rust (nearest `.git`), the prefix is computed from canonical paths, and the filter verdict is cached per work tree, keyed by the mtimes of `.git/config` and `config.worktree` and trusted for at most 10 s (includes can live elsewhere). Listing statuses come from a background worker; the `gG` filter computes them synchronously because it needs them.
- **Diff viewer (4.10):** all five phases. `src/diff.rs` (parser, runner, numstat, word diff, rows, rendering), `src/diffview.rs` (state, keys, file pane). The comparison base before the first commit is the empty tree. Viewed marks key on path + a fingerprint of the file's numstat line, size and mtime (no diff run per listed file). `P` sends the hunk; `^l` reloads.
- **Ambitious ideas (5.x):** `:history` overlay (persisted in `state_dir/journal.json`, `u` reviews undoing any batch); conflict report with `c` (number clashing names: `report 2.pdf`) and `o` (jump); `:plan-copy` and `:plan > file` (never overwrites); `:recipe` with built-ins and `[recipes]` from the config (one undo step); `gd` metadata level (owner:group, xattrs `@`, broken links); the **Dash** finder tab; context send (`P` in the viewer and diff view, `alt-s` for grep lines) goes to agents as a bracketed paste and to the clipboard when the origin pane is a shell.

Items below marked **Review note** were checked against the code; the note records corrections to the "current state" or the design decision needed before implementing.

---

## 1. Added Functionality

### A. Buffer & Vim Power-Editing
1. **Sequential Numbering / Counter Token in `:%s` and `:prefix` / `:suffix`** ✅
   - **Current state:** `substitute` (`src/vim.rs`) and `Vim::ex_range` (`src/vim.rs`) support regex capture groups (`$1` / `\1`), static prefixes, and static suffixes.
   - **Suggestion:** Support a counter token such as `{#}` / `{03#}` (or `\=i`) across a range (e.g., `:'<,'>s/^/{02#}_/` or `:%s/IMG_\d+/photo_{03#}/`). Sequential renumbering of screenshots, migrations, or test fixtures is one of the most common batch-rename tasks that plain regex captures cannot express.
   - **Review note:** Decide before implementing: does the counter advance per match or per line (`g` flag with several matches per line), what is the start value / step syntax, and how to write a literal `{`. Should apply to `:prefix` / `:suffix` too.
2. **`:g/pat/cmd` & `:v/pat/cmd` (Global Filter Operations)** ✅
   - **Current state:** Range commands in `Vim::ex_range` (`src/vim.rs`) operate on contiguous line ranges (`%`, `'<,'>`, `n,m`).
   - **Suggestion:** Add `:g/pattern/d` (delete all matching lines, e.g. `:g/\.bak$/d` or `:g/\.DS_Store/d`), `:v/pattern/d` (keep only matching lines), and `:g/pattern/s/.../.../` in `Vim::ex` (`src/vim.rs`). This pairs naturally with the live preview infrastructure (`sub_base`) already in `Vim::live_substitute`.
   - **Review note:** Valid. `:g/pat/d` deleting lines means trashing files on `:w`, so the live preview must make the deletions obvious. Scope: `d`, `s`, and possibly `normal` — keep to `d` and `s` first.
3. **Preserve Source Entry Lookups on Edited Lines (`Buffer::cur_source_entry`)** ✅
   - **Current state:** `Buffer::cur_entry` (`src/buffer.rs`) still returns `None` once a line's source name no longer matches the entry name. As a result, while a line is renamed in the buffer, its preview disappears and commands that expect an existing source entry can fall back to the current directory.
   - **Suggestion:** Add a helper `Buffer::cur_source_entry` that looks up `l.id` in `Store` (`src/entry.rs`) regardless of whether `l.name()` was edited, so previews, `gy`, `gi`, and `:chmod` continue to work on lines with pending renames.
   - **Review note:** Partly inaccurate. `:chmod` and `gi` already act on line ids (`Action::Chmod { ids }`, `Action::Send`), so they work on renamed lines. Only the explorer preview (`ui.rs`, `draw_explorer`) and `gy` (`Action::YankPath`) are affected. `open_current` relies on `cur_entry` returning `None` for edited lines, so add the new helper rather than changing `cur_entry`.

### B. Finder & Grep Workflow
4. **Live Grep / Finder → Editable Buffer ("Quickfix / Oil-Grep" Mode)** — ⏸ deferred
   - **Current state:** In `Finder` (`src/finder.rs`), `^space` marks multiple files or grep hits, marks keep line numbers, `^s` sends them, and `^o` reveals a single file in the explorer.
   - **Suggestion:** Add a binding in Finder (e.g., `^q` or `alt-e`) that loads the unique files from the current Finder/Grep results (or `marked` files) into a virtual multi-directory Oil buffer where each line displays `rel/path/to/file.rs`. Because `Store` (`src/entry.rs`) and `plan::build` (`src/plan.rs`) already track full `PathBuf`s per entry ID, users can batch-rename, move, or delete files across the entire repository from a fuzzy query or ripgrep search.
   - **Review note:** Large. `Buffer` is built around one directory (`b.dir`): line text is a name relative to it, and `plan::build` joins it to `b.dir`. A multi-directory buffer needs a new line/path model throughout `buffer.rs`, `plan.rs`, and `draw_explorer`. Defer.
5. **Archive Extraction / Creation (`:extract` / `:archive`)** ✅
   - **Current state:** `preview::special` (`src/preview.rs`) previews `.zip` and `.tar.*` contents.
   - **Suggestion:** Support `:extract` on an archive entry to unpack it into a subdirectory, and `:zip name.zip` / `:tar name.tar.gz` on a visual selection (`'<,'>`) to pack selected entries.
   - **Review note (safety):** Extracting untrusted archives risks zip-slip (`../` entries) and symlink escapes. Validate every entry path stays inside the target folder; never extract over existing files.

---

## 2. Visualization & UI Polish

### A. Explorer & Buffer Rendering
1. **Highlight Search Matches (`/`) Inside Explorer & Live `:s` Spans** ✅
   - **Current state:** The built-in file viewer highlights `/` search matches using `mark_matches` (`src/ui.rs`), and the confirm dialog highlights character diffs using `diff_spans` (`src/ui.rs`). However, in `draw_explorer` (`src/ui.rs`), typing `/pattern` only moves the cursor to the matching line without highlighting the matched substring, and typing `:%s/pat/rep/` replaces the line text in-place without highlighting the replaced span.
   - **Suggestion:** Reuse `mark_matches` in `draw_explorer` when `vim.mode` is `Cmd { kind: '/', .. }` (or when `vim.search` is active), and highlight the substituted spans in `th.yellow` / `th.green` during `Vim::live_substitute` (`src/vim.rs`).
2. **Breadcrumb / Parent Context or Miller-Column Option** ✅
   - **Current state:** `draw_explorer` (`src/ui.rs`) splits the body 48% / 52% between the current directory and the preview pane.
   - **Suggestion:** In wide popups (`width >= 90`), offer an optional narrow parent-directory column on the left (15% / 40% / 45%, toggled via `gw` or config) or interactive clickable path segments in the top header bar (`head` in `draw_explorer`) so mouse users can click any ancestor directory in `~/Projects/herdr-oil/src/` to jump straight to it.

### B. Preview & Viewer Enhancements
3. **PDF Metadata / Text Previews** ✅
   - **Current state:** `preview::special` (`src/preview.rs`) handles archives, SQLite schemas, binary plists, image metadata, and generic binary summaries, but PDFs still fall through to the generic binary path.
   - **Suggestion:** Support `.pdf` metadata/text preview via `pdftotext`, `pdfinfo`, or `mdls` (macOS) in `preview::special`.

---

## 3. Ease of Use & Performance

### A. Ergonomics & Workflow
1. **Unified Line-Editing Keys Across `:` Command Mode, `/` Search, and Viewer** ✅
   - **Current state:** `App::finder_key` (`src/app.rs`) supports `^w` (delete word back), `^a` / `Home`, and `Alt-e` / `End`. By contrast, `Vim::cmd_key` (`src/vim.rs`) and `App::viewer_key` (`src/app.rs`) only handle `Backspace` and `^u`.
   - **Suggestion:** Extract a small shared line-input helper so `^w`, `^a`, `^e`, `Home`, `End`, and `Left` / `Right` cursor movement work identically across Finder, `:` command mode, `/` search, and Viewer `/` search.
   - **Review note:** `Vim::cmd_key` already handles `Left` / `Right` (and now `Up` / `Down` / `^p` / `^n` for history). The Viewer `/` prompt is the weakest: no cursor at all (`push` / `pop` only).
2. **Interactive Confirm Modal (Jump to Entry / Drop Single Op)** ✅
   - **Current state:** When edits span multiple directories, the header shows `(N other dirs)`, and `:w` opens `Overlay::Confirm` (`src/app.rs`), which lists operations with `j` / `k` scroll but does not allow acting on individual items.
   - **Suggestion:** Add a cursor selection inside `Overlay::Confirm` so pressing `gd` / `⏎` jumps directly to that directory/line in the Explorer, or pressing `x` / `d` drops that single operation from the plan before applying.
3. **Expanded Tab Completion (`:chmod`, `:sort`, and Buffer Filenames)** ✅
   - **Current state:** `Vim::complete` (`src/vim.rs`) completes ex command names and paths after `:cd` / `:e`.
   - **Suggestion:** Extend `Vim::complete` to complete `:sort ` arguments (`name`, `mtime`, `size`, `ext`), `:chmod ` presets (`+x`, `-x`, `644`, `755`), and current-buffer filenames when typing `:s/` or `:touch `.

### B. Performance & Responsiveness
4. **Background Preview Rendering** ✅
   - **Current state:** `Cache::get` (`src/preview.rs`) runs `bat`, `git diff`, `tar`, or `unzip` synchronously inside `ui::draw` while holding the terminal frame. Holding down `j` / `k` in the Explorer or Finder spawns a subprocess on every key repeat before the frame can draw.
   - **Suggestion:** Debounce or run `preview::render` (`src/preview.rs`) on a background thread so `j` / `k` cursor movement stays 60fps even when `bat` or `git diff` takes 30–50ms.
   - **Review note:** Valid. Needs a worker thread plus a request generation so stale results are dropped, and a redraw trigger when a result lands (the loop otherwise idles 250ms in `event::poll`). Show the previous preview or a placeholder meanwhile.
5. **Cache `plan::build` Across Frames in `draw_explorer`** ✅
   - **Current state:** Whenever any buffer is modified, `draw_explorer` (`src/ui.rs`) runs `plan::build` (`src/plan.rs`)—including `std::fs::symlink_metadata(dst)` inside `plan::validate`—on **every UI draw tick** (including spinner ticks and cursor movements).
   - **Suggestion:** Cache the computed `Plan` on `App`, keyed by the sum of `Buffer::edits` across `app.buffers` (and `live_substitute` state), so `plan::build` and its `symlink_metadata` syscalls only run when buffer text actually changes.
   - **Review note:** Low priority. The loop draws once per event or every 250ms when idle (not on spinner ticks), and validation is one `symlink_metadata` per op. The proposed key is unsound: editing inside an already-edited line leaves the edit count unchanged, so the cached plan would go stale. Use a per-buffer generation counter bumped on every mutation if this is ever needed.

---

## 4. Git Integration Improvements

### A. Bug Fixes & Edge Cases
1. **Enable Rename Detection in `git status` (`R` Status)** ✅
   - **Current state:** `status_records` (`src/entry.rs`) passes `--no-renames` to `git status`, even though lines 441–443 and `status_letter` explicitly parse `R` (renamed) and `C` (copied) records. As a result, `git mv` shows up as separate `A` and `D` entries instead of `R`.
   - **Suggestion:** Remove `--no-renames` (or pass `--find-renames`) so staged renames properly appear as `R`.
   - **Review note:** With renames on, the source path is skipped (`it.next()`), so a directory a file was `git mv`'d out of loses its dirty marker when the move crosses directories. Aggregate the source path too, and check rename detection cost in large repos.
### B. Richer Git Visualization
2. **Distinguish Staged vs. Unstaged vs. Partially Staged (`XY` Status)** ✅
   - **Current state:** `status_letter` (`src/entry.rs`) collapses the two-character porcelain `XY` code into a single `char` (`M`, `A`, `D`, `?`, `!`). Users cannot tell whether changes are staged (`M `), unstaged (` M`), or both (`MM`).
   - **Suggestion:** Preserve both `X` (index) and `Y` (worktree) state so `draw_explorer` and `draw_finder` can color-code or render two-column markers:
     - **Green (`M`, `A`, `D`, `R`):** Staged in index only
     - **Yellow `M` / Red `D`:** Unstaged worktree changes
     - **Peach `M` (or `±`):** Both staged and unstaged changes (`MM`, `AM`)
   - **Review note:** Changes the value type of `Buffer::git` and the Finder's git map from `char` to an `XY` pair, touching `entry.rs`, `finder.rs`, and both draw functions. Directory aggregation needs a rule for mixing staged and unstaged children.
3. **Branch Name & Ahead/Behind Badge in Explorer Header** ✅
   - **Current state:** The Explorer header shows only the directory path, pending buffer edits, and sort mode.
   - **Suggestion:** Pass `-b` to `git status --porcelain=v1 -b -z` to parse the `## branch...upstream [ahead N, behind M]` header at zero extra subprocess cost, and display a compact branch badge (e.g. ` main ↑1`) in `draw_explorer`.
   - **Review note:** Valid. `status_records` returns `None` in repositories that define their own filters, so the badge must be absent there too; detached HEAD and unborn branch headers need handling.
4. **Explorer Diff Toggle (`gD`) & Inline Diff Gutter Indicators** ✅
   - **Current state:** `draw_preview` (`src/ui.rs`) always passes `diff = false` in the Explorer view (`diff = true` is only used in the Finder's Git tab).
   - **Suggestion:** Add `gD` in the Explorer to toggle the side preview between file content and `git diff HEAD` (or staged vs. unstaged diff), and optionally render subtle `│` gutter bars (green added, yellow modified, red deleted) next to line numbers in normal file previews.
   - **Review note:** Viewer `d` is done. `gD` should open the diff view from item 10 rather than only switching the side preview. Gutter bars can reuse item 10's parser (`old_no` / `new_no` per hunk line).
### C. Git Workflow & Navigation Actions
5. **Jump Between Modified Entries (`]c` / `[c` or `]g` / `[g`)** ✅
   - **In Explorer:** Jump the cursor directly to the next/previous entry (file or folder) with a non-clean git status (`M`, `A`, `?`, `U`, `D`).
   - **In Viewer:** Jump between diff hunks when viewing a file or diff.
   - **Review note:** Valid and fairly small for the Explorer (`b.git` is per buffer). Viewer hunk jumps are part of item 10 (`]c` / `[c`).
6. **Stage, Unstage, and Discard Changes (`:stage`, `:unstage`, `:restore`, Finder `^a`)** ✅
    - **In the Finder `Git` tab (`␣s`):** Bind `^a` (or `gs`) to toggle staging on the selected/marked file(s) (`git add --` / `git restore --staged --`) and refresh the list and diff preview in-place; bind `alt-d` to discard unstaged worktree changes after a confirmation prompt.
    - **In the Explorer:** Support `:stage`, `:unstage`, and `:restore` on the cursor line or visual selection (`:'<,'>stage`).
    - **Review note (safety):** `git add` runs clean filters: reuse the `repo_defines_filters` guard and the hooks/fsmonitor-off flags from `status_records`. `:restore` discards work irreversibly and must confirm.
7. **Filter Explorer to Changed Entries Only (`:git-filter` or `gG`)** ✅
    - **Suggestion:** Add a toggle in the Explorer that filters the current directory buffer to only entries with active git status markers (including directories that contain dirty children), making it effortless to focus on modified files in large directories.
8. **File / Directory Commit Log Preview (`gl`)** ✅
    - **Suggestion:** Pressing `gl` on an entry in the Explorer or Viewer opens `git log --oneline --graph --decorate -n 50 -- <path>` in the Viewer so users can inspect recent commits touching that file or folder.
    - **Review note:** Valid. The Viewer renders files, so it needs a "command output" source (cached like `git_diff`) rather than a path; run with the same no-hooks/no-pager flags.

### D. Git Subprocess Performance
9. **Consolidate the 3 Git Subprocesses into 1 Cached Call & Run Asynchronously** ✅
    - **Current state:** `status_records` (`src/entry.rs`) spawns **three** synchronous `git` processes every time a directory is entered or refreshed:
      1. `git config --list --show-scope --includes --name-only` (in `repo_defines_filters`)
      2. `git status --porcelain=v1 -z ...`
      3. `git rev-parse --show-prefix`
    - **Suggestion:**
      - Cache `repo_defines_filters` and the repository root (`git rev-parse --show-toplevel`) per repo in `App`, and compute the relative prefix in pure Rust (`dir.strip_prefix(&repo_root)`), reducing 3 subprocesses to **1**.
      - Run that single `git status` call on a background worker thread, updating `Buffer::git` asynchronously so directory navigation is instant even in large repositories.
    - **Review note (safety):** `repo_defines_filters` is a security guard, not just overhead. Cache it per repository root only, and remember `includeIf` makes config depend on the directory. Computing the prefix in Rust needs canonical paths (`/tmp` vs `/private/tmp` on macOS), or `strip_prefix` silently drops every record.

### E. Spec: GitHub-Style Diff Viewer (Unified & Split)
10. **Structured diff view replacing the coloured `git diff` dump** ✅
    - **Mockup:** https://claude.ai/artifact/NFpB4CXY6AtweZNhbQsHh5 — boards "Files changed · unified" and "Split view (s)". (The "Narrow popup · hunk staging" board is not part of this spec.)
    - **Current state:** `preview::git_diff` (`src/preview.rs`) runs `git diff --color=always HEAD -- <path>` and pipes the ANSI output through `parse_ansi`. There is no notion of hunks, line numbers, or pairs of changed lines, so nothing can be navigated, folded, or laid out side by side.

    **Data model** (new `src/diff.rs`)
    - Run git with the same precautions as today (`repo_defines_filters` guard, hooks/fsmonitor/external diff/textconv off, `GIT_OPTIONAL_LOCKS=0`) but `--no-color`, and parse the unified output into:
      `DiffFile { path, old_path, status, binary, hunks: Vec<Hunk> }`,
      `Hunk { old_start, old_len, new_start, new_len, header /* text after @@ … @@ */, lines: Vec<DLine> }`,
      `DLine { kind: Ctx | Add | Del, old_no: Option<usize>, new_no: Option<usize>, text }`.
    - Handle `\ No newline at end of file`, rename/copy headers (`rename from` / `rename to`), new/deleted files, mode-only changes, and `Binary files … differ` (render "binary file changed" plus both sizes).
    - Untracked files (Git tab `?`): show as all-added via `git diff --no-index -- /dev/null <path>` (exit code 1 means "differs", not failure).
    - Strip or escape control characters in line text before rendering (content is no longer coming through `parse_ansi`).
    - Unit tests: hunk-header parsing and line numbering, no-newline marker, rename headers, binary, untracked.

    **Unified layout** (default)
    - Per line: old number │ new number │ sign │ code. Numbers in `th.dim`; added lines get a green-tinted background and a slightly stronger tinted gutter, removed lines the red equivalents; context lines are untinted.
    - Hunk header row: `⋯  @@ -270,27 +272,33 @@ fn validate(plan: &mut Plan) {` on a faint `th.blue` tint, full width.
    - Between hunks a fold row: `↕  Expand 204 unchanged lines   (z)`.
    - File header row: status letter, `dir/` in `th.subtext` + **name**, `+N −M`, and a five-block stat bar (green blocks = round(5·add/(add+del)), rest red).
    - Tints: blend `th.green` / `th.red` into the background at ≈14% (line), ≈20% (gutter), ≈30% (changed words). The theme has no background colour, so add an optional `background` key under `[theme.custom]` and default to `#222222`. Terminals without truecolor: no backgrounds, coloured signs and numbers only.

    **Word-level highlights**
    - Inside each change block (run of `-` lines followed by `+` lines), pair the i-th removed line with the i-th added line; tokenise both (whitespace runs, string literals, identifiers, numbers, single punctuation), take the token LCS, and give tokens outside it the stronger tint. Whitespace tokens are highlighted only when both neighbours are.
    - Skip pairing when a line is over ~500 chars or a block is over ~200 lines; compute lazily for visible rows and cache.

    **Syntax colour**
    - Phase 1: none (code in `th.text`, which already reads well on the tints).
    - Phase 2: highlight both sides with `bat --color=always` (old side from `git show <base>:<path>`, new side from the worktree or `git show :<path>` for the index) and pick lines by `old_no` / `new_no`, then lay the diff tints under bat's foreground colours.

    **Split layout** (`s` toggles; config `diff_layout = "auto" | "unified" | "split"`, `auto` = split when the body is ≥ 160 columns)
    - Two halves with column titles (`HEAD` | `working tree`, or the bases of the current comparison). Context lines appear on both sides; in a change block removed lines go left and added lines right, row-aligned in order, with an empty filler cell (`#262629`-ish) where one side runs out.
    - Each half: number │ sign │ code, clipped to its width; `h` / `l` scroll both halves together.

    **Multi-file "Files changed" view**
    - `⏎` in the Finder Git tab opens the viewer on the selected file with the rest of the changed files loaded (the Git tab's item list). With more than one file and width ≥ 120 columns, a left pane (~30 columns) lists them as a tree: status letter, name, `+a −d`, ✓ when viewed; current file highlighted with an accent edge.
    - Top bar: branch, `↑ahead ↓behind` (shares the parsing with item 3), `N files changed +A −D`, total stat bar, the comparison (`working tree` = HEAD→worktree, `staged` = HEAD→index, `unstaged` = index→worktree), and `viewed k/N`.
    - Viewed marks live in memory for the session, keyed by path plus a hash of that file's diff, so a file that changes again drops its ✓.

    **Navigation & keys** (inside the diff view; the existing viewer keys `j k ␣ b ^d ^u g G / n N y o e ^v ^x ^t q` keep their meaning)
    - A cursor row (selection background) so actions have a target.
    - `]c` / `[c` next/previous hunk · `⇥` / `⇧⇥` next/previous file · `s` unified/split · `w` ignore whitespace (`-w`) · `z` expand the fold under the cursor by 20 lines, `Z` expand all (re-run that file with a larger `-U`) · `v` toggle viewed · `c` cycle comparison · `⏎` open `$EDITOR` at the cursor's new-side line · `p` send `path:line` to the origin pane · `d` back to the plain file view.
    - Footer hints in the existing style (`]c [c hunk · ⇥ file · s split · w whitespace · z expand · v viewed · ⏎ open at line · q back`).
    - Hunk staging (`S` / `U`) is out of scope here: it comes with item 6 and its safety rules.

    **Entry points**
    - Finder Git tab `⏎` (multi-file), Viewer `d` (single file; today's toggle), Explorer `gD` (item 4: opens this view for the cursor entry; on a directory, all changed files under it).
    - The side preview in the Git tab keeps a compact unified rendering (no file pane, no split).

    **Performance**
    - Parse once per (file, comparison, whitespace, context) and cache; render only visible rows. One `git diff` per file on demand, not for all files up front; the stat counts for the file pane come from a single `git diff --numstat` call.

    **Phases**
    1. `diff.rs` parser + unified layout in the Viewer with tints, hunk headers, folds, cursor, `]c` / `[c`.
    2. Word-level highlights.
    3. Split layout, `auto` switching, `h` / `l`.
    4. Multi-file view from the Git tab: file pane, `⇥`, stat bars, viewed marks, comparison cycling.
    5. Fold expansion (`z` / `Z`), whitespace toggle, bat syntax colour.

---

## 5. Additional Ambitious Ideas

### A. Safety, Review & Undo
1. **Trash / Apply History Browser (`:history` or `:trash`)** ✅
   - **Suggestion:** Add an overlay that lists recent `:w` batches, deleted items moved to Trash, undoability, permanent-delete gaps, and restore actions.
   - **Why:** `U` is powerful but invisible. A history browser would make destructive operations feel inspectable and recoverable.

2. **Conflict-Fix Actions in the Error / Confirm Flow** ✅
   - **Suggestion:** When `:w` reports collisions, offer single-key fixes: append a counter, reveal the conflicting entry, drop one operation, or jump to the source line.
   - **Why:** The current safety model prevents damage, but the user still has to manually navigate back and repair the buffer.

3. **Safe Dry-Run Export** ✅
   - **Suggestion:** Add `:plan-copy` or `:plan > path` to copy/export the pending plan as readable text.
   - **Why:** Useful for code reviews, agent handoff, audit trails, and “show me what this will do” workflows.

### B. Rename & Batch Operation Power
4. **Named Operation Recipes** ✅
   - **Suggestion:** Support reusable rename recipes such as `:recipe slugify`, `:recipe screenshots`, `:recipe kebab`, or user-defined recipes in config.
   - **Why:** Many batch rename patterns repeat across projects and should not require retyping complex regexes.

5. **Counter-Aware Conflict Resolution** ✅
   - **Suggestion:** Pair the counter token feature with automatic collision repair, e.g. `report_{03#}.pdf` or “append counters to all conflicting destinations.”
   - **Why:** Counter renaming and collision repair solve the same real-world batch-renaming pain.

### C. Richer Explorer Modes
6. **File Metadata Mode** ✅
   - **Suggestion:** Add a toggle for owner/group, xattrs, executable bit, symlink health, broken links, and extended file attributes.
   - **Why:** `details` is already useful; a deeper inspection mode would turn herdr-oil into a serious maintenance tool.

7. **Project Dashboard Finder Tab** ✅
   - **Suggestion:** Add a tab that combines Git changes, recent files, marked paths, and pending Oil edits into one triage view.
   - **Why:** The plugin now has several powerful modes. A dashboard would make it feel like a workspace cockpit rather than separate tools.

8. **Context Send to Agents** ✅
   - **Suggestion:** Extend `send` beyond paths: send selected grep lines, nearby context, preview snippets, or Git diff hunks to the origin pane.
   - **Why:** Herdr users often work with coding agents; sending exactly the useful context is more valuable than sending only filenames.

### D. Recommended Implementation Order
1. Diff viewer (item 4.10) phases 1–3: parser, unified layout, word highlights, split view.
2. Diff viewer phase 4–5 (multi-file view from the Git tab), then Explorer `gD` and `]g` / `[g` jumps on top of it.
3. `:g` / `:v` global commands and the counter token.
4. Branch badge, then staged/unstaged `XY` status.
5. Move preview rendering off the draw path, then consolidate git subprocesses.
6. Add the history/trash browser once the Git and preview experience feels solid.
