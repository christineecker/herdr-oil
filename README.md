# herdr-oil

An [oil.nvim](https://github.com/stevearc/oil.nvim)-style file explorer and a
fuzzy finder for [herdr](https://herdr.dev), in a popup like herdr-hop.

The directory listing **is a text buffer**. Edit it with vim keys, then `:w`:
herdr-oil diffs the buffer against the disk, shows the plan, and applies it.

| you do                                  | herdr-oil does          |
|-----------------------------------------|-------------------------|
| change a line (`cw`, `cin`, `A`…)       | rename                  |
| `o` + `name` / `name/` / `a/b/c.txt`     | create file / dir / nested |
| `dd`                                    | delete (to Trash)       |
| `dd`, go elsewhere (`-`, `⏎`), `p`       | move                    |
| `yy`, `p`, edit the name                | copy                    |

Edits in several directories are kept and applied together. Swaps (`a`↔`b`),
case-only renames on APFS, "move the child out, delete the parent" and
"rename a dir while editing inside it" are handled. Nothing is ever
overwritten; clashes are reported before anything runs.

The finder (`⇥` from the explorer, or its own key) has four modes:
**Files** and **Dirs** (parallel walk honouring `.gitignore`, nucleo fuzzy
matching, fzf query syntax), **Grep** (live ripgrep), and **Recent** (files you
opened through herdr-oil). Previews use `bat` when installed.

## Install

```sh
herdr plugin link /path/to/herdr-oil      # builds with cargo
```

Bind the actions in `~/.config/herdr/config.toml`, then `herdr server reload-config`:

```toml
[[keys.command]]
key = "prefix+d"
type = "plugin_action"
command = "herdr-oil.explorer"

[[keys.command]]
key = "prefix+shift+f"
type = "plugin_action"
command = "herdr-oil.finder"

[[keys.command]]
key = "prefix+slash"
type = "plugin_action"
command = "herdr-oil.grep"
```

Actions: `explorer`, `finder`, `grep`, `recent`. Each opens the popup at the
focused pane's working directory.

## Opening files

`⏎` follows `open` in the config. The default `view` shows the file inside the
oil window: syntax-highlighted, scrollable (`j k ␣ b ^d ^u g G`), `e` edits it
in `$EDITOR` inside the popup, `q` goes back. A preview pane sits next to the
listing by default (`^p` toggles it).

With `open = "auto"`, `$EDITOR file` is typed into the pane you came from when
it sits at a shell prompt, otherwise a split opens to its right. Agents are
never typed into, and names with control characters are never typed at all.
Explicit keys, from the listing or the viewer:

| key  | target |
|------|--------|
| `^v` | split right (`exec $EDITOR`, the split closes with the editor) |
| `^x` | split below |
| `^t` | new herdr tab |
| `^e` | inside the popup; you return to the explorer afterwards |
| `gx` / finder `^g` | system opener |

Line numbers from grep are passed as `+N` (vim, nvim, nano, micro, emacs, kak),
`file:N` (helix, zed, subl) or `-g file:N` (VS Code family).

## Keys

`g?` in the popup shows everything. Highlights:

| explorer | |
|---|---|
| `⏎` `-` `_` `~` | view file / enter dir · parent · start dir · home |
| `:w` `^s` `ZZ` | review & apply · apply and quit |
| `q` `esc` | close (asks first when edits are unsaved; esc cancels a pending key first) |
| `u` `^r` | undo · redo (per directory) |
| `g.` `gs` `gd` `^p` | hidden · sort · sizes/details/off · preview |
| `gy` | copy absolute path |
| `:cd p` `:touch a b` `:mkdir d` `:sort mtime` `:e!` | |
| `⇥` `␣f` `␣g` `␣d` `␣r` | finder: files · grep · dirs · recent |

| viewer (`⏎` on a file) | |
|---|---|
| `j` `k` `␣` `b` `^d` `^u` `g` `G` | scroll |
| `e` | edit in `$EDITOR` inside the popup, back to the viewer after |
| `^v` `^x` `^t` `o` `y` | split right · below · tab · system app · copy path |
| `q` `esc` | back to the listing or finder |

| finder | |
|---|---|
| `⇥` `⇧⇥` | cycle Files / Dirs / Grep / Recent |
| `^o` | reveal the selection in the explorer |
| `^space` | mark several files, `⏎` opens them all |
| `^r` | root: cwd → repository root → parent |
| `alt-h` `alt-i` `alt-p` | hidden · ignored · preview |

Vim support: counts, `h j k l w b e W B E 0 ^ $ gg G f F t T ;`, operators
`d c y` with motions and text objects `iw aw iW aW`, plus `in`/`an` for the
file-name stem and extension (`cin` renames `report.md` keeping `.md`),
`x X s S C D r J ~ p P o O i a I A`, visual line mode, `/` search with `n N`.

## Config

`$(herdr plugin config-dir herdr-oil)/config.toml`, see
[`config.example.toml`](config.example.toml): editor, open target, trash vs
permanent delete, hidden files, details, preview, icons, git status, confirm,
stay-open, ignore rules, finder cap.

## CLI

```sh
herdr-oil [DIR|FILE]          # explore (FILE: its dir, cursor on it)
herdr-oil --finder | --grep | --recent
herdr-oil --pick              # print chosen file(s) instead of opening
```

## Development

```sh
cargo test        # planner, apply engine (on temp dirs), vim engine, finder, parsers
cargo build --release
```

Layout: `buffer.rs` (lines with hidden ids) · `vim.rs` (modal editing) ·
`plan.rs` (buffer diff → ops + validation) · `apply.rs` (staged execution) ·
`finder.rs` (nucleo + ignore + rg) · `preview.rs` (bat/ANSI) · `open.rs`
(editor hand-off) · `launch.rs` (herdr CLI) · `ui.rs` · `app.rs`.

## Project page

`docs/index.html` is a self-contained overview (screens, interactive model of
the buffer → plan rules, keys, source) for GitHub Pages: Settings → Pages →
Deploy from branch → `main` / `/docs`. Rebuild it after code changes with
`python docs/src/build_page.py` (runs `cargo test` for the test list; the
screens are ANSI captures from `herdr pane read --format ansi` in
`docs/src/shots/`). Replace `OWNER` in the clone URL in
`docs/src/page.tpl.html` once the repo exists.

## Acknowledgements

herdr-oil exists because of [oil.nvim](https://github.com/stevearc/oil.nvim)
by Steven Arcangeli. The central idea comes from oil: the directory listing is
an ordinary editable buffer where every line carries a hidden entry id, and
saving diffs that buffer against the disk and asks before applying creates,
renames, moves, copies and deletes. Key choices like `-` for the parent
directory and `g.` for hidden files follow oil too.

herdr-oil is an independent Rust reimplementation for herdr popups, not a
port. No oil.nvim code is used. Please star and support the original.
