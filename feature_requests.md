# `herdr-oil` Feature Requests

Open requests only. Implemented items are removed from this file once they land; the README describes what exists.

Items marked **Review note** were checked against the code; the note records corrections to the "current state" or the design decision needed before implementing.

---

## 1. Finder

1. **Live Grep / Finder → Editable Buffer ("Quickfix / Oil-Grep" Mode)** — ⏸ deferred
   - **Current state:** In `Finder` (`src/finder.rs`), `^space` marks multiple files or grep hits, marks keep line numbers, `^s` sends them, and `^o` reveals a single file in the explorer.
   - **Suggestion:** Add a binding in Finder (e.g., `^q` or `alt-e`) that loads the unique files from the current Finder/Grep results (or `marked` files) into a virtual multi-directory Oil buffer where each line displays `rel/path/to/file.rs`. Because `Store` (`src/entry.rs`) and `plan::build` (`src/plan.rs`) already track full `PathBuf`s per entry ID, users can batch-rename, move, or delete files across the entire repository from a fuzzy query or ripgrep search.
   - **Review note:** Large. `Buffer` is built around one directory (`b.dir`): line text is a name relative to it, and `plan::build` joins it to `b.dir`. A multi-directory buffer needs a new line/path model throughout `buffer.rs`, `plan.rs`, and `draw_explorer`. Defer.
   - **Extension: editable hit lines.** In the grep tree (`alt-t`), `alt-e` makes the hit lines editable as well: editing a hit line rewrites that line in its file, editing a file row renames the file, `dd` on a file row deletes it, and `:s`, visual mode, counters and dot-repeat work across all hits. Folder rows, line numbers and indentation stay read-only. `dd` on a hit row resets that line; it does not delete the line from the file. The preview shows the selected file's pending diff. Prior art: wgrep, quickfix-reflector, grug-far.
   - **Extension: one review for line edits and file ops.** Add `Op::Patch { path, line, before, after }` to `plan.rs`. The `:w` dialog lists patches next to renames and deletes. `apply.rs` writes each file's patches once, through a temp file, before any `move_path`, so every path is still where rg found it. A patch whose line no longer reads `before` is skipped and reported. The dialog also warns when a rename would break a symlink and offers `Op::Retarget`. Binary files and lines too long for the pane are listed but stay read-only.

2. **Projects tab: features not yet ported from herdr-hop**
   - Templates and `gh repo create`, the GitHub browser and clone, worktree create, start agent, lazygit split, zoxide ranking, `[recipe]`.

3. **One popup: the explorer on the empty prompt**
   - **Current state:** Step 1 is in: the empty prompt sits over the listing, the two focus states work (INSERT / NORMAL badge), `esc` no longer closes, `/` `+` `<` `@` from the list focus the prompt, and results are read-only (`⏎` in the result list opens the row's folder in the listing; `]` `[` took over group jumps from `⇥`). `?` is now the listing's name search. The Dash rows are on `␣h`.
   - **Suggestion:** Merge them. On an empty prompt the finder's list is the explorer's editable listing of the current folder. Typing filters names across subfolders, and `/pattern` shows the grep tree (`alt-t`). Rows stay editable at every step (item 1 and its extensions), and pending edits survive changing the query. `prefix+d` opens the popup with focus on the list, and `prefix+/` opens it with focus on the prompt and `/` typed. The first entry in the sources row becomes `› files`.
   - **Two focus states:** INSERT (prompt): letters type; `↑` `↓` `⏎` still work; `esc` moves focus to the list. NORMAL (list): every explorer key; `esc` clears the query; `q` closes the popup (asking first when edits are pending); `/` `+` `<` `@` focus the prompt with that sigil typed; `⇥` toggles focus. The footer badge names the state. `h` `l` fold the grep tree in NORMAL (as `←` `→` do today) and are letters in INSERT.
   - **Changes from today:** `esc` no longer closes or goes back. `⇥` toggles focus instead of switching views. In the list, `/` starts a grep instead of vim's name search; the prompt's name filter replaces that search. The Dash rows move from the empty prompt to `␣h`.
   - **Review:** One `:w` lists every pending edit, grouped by where it was made (the listing, a name query, a grep query). Patches run first, then moves and renames, then deletes.
   - **Decided:** A row whose edit stops matching the query stays visible until `:w`.
   - **Decided:** The preview heading keeps its diff overview in every state: git letter, name, size and age on the left, `+N −M` and the bar on the right (`draw_preview_head`), with the diff in the body. For pending line edits the heading uses the same `+N −M` and bar, counted against the file on disk.
   - **Open question:** After `:w`, should the popup return to the listing or stay on the results?
   - **Build order:** (1) done; (2) multi-folder editing (item 1), so rows stay editable under a query, then `Op::Patch`.
   - **Mockup:** walkthrough of seven states plus a key table (claude.ai artifact "Oil One Popup").
