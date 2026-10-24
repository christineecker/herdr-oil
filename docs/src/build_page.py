"""Build docs/index.html (GitHub Pages) from the ANSI captures in shots/.

    python docs/src/build_page.py        # runs cargo test, rewrites docs/index.html

Captures come from `herdr pane read <pane> --source visible --format ansi`.
"""
import glob
import html
import json
import os
import re
import sys

SP = os.path.dirname(os.path.abspath(__file__))
SHOTS = os.path.join(SP, "shots")
SRC = os.path.abspath(os.path.join(SP, "..", ".."))

FG, BG = (247, 241, 255), None
FOLDERS = {0xF024B, 0xF07C, 0xF07B, 0xF08DE, 0xE5FB, 0xE5FA, 0xE5FD}
REPL = {0xF0349: "⌕", 0xF02DA: "◷", 0xF0337: "↪", 0xF1A5: "↪", 0xF0214: "▤"}


def glyph(ch):
    o = ord(ch)
    if 0xE000 <= o <= 0xF8FF or o >= 0xF0000:
        if o in FOLDERS:
            return "▸"
        return REPL.get(o, "◆")
    return ch


SGR = re.compile(r"\x1b\[([0-9;:]*)m")


def ansi_to_html(text, rows=None):
    lines = text.replace("\r", "").split("\n")
    while lines and not lines[-1].strip():
        lines.pop()
    out = []
    for line in lines[:rows]:
        st = {"fg": None, "bg": None, "b": False, "dim": False, "rev": False, "it": False}
        buf, html_line = "", []

        def flush():
            nonlocal buf
            if not buf:
                return
            fg, bg = st["fg"], st["bg"]
            if st["rev"]:
                fg, bg = (bg or (42, 42, 42)), (fg or FG)
            css = []
            if fg:
                css.append("color:rgb(%d,%d,%d)" % fg)
            if bg:
                css.append("background:rgb(%d,%d,%d)" % bg)
            if st["b"]:
                css.append("font-weight:700")
            if st["dim"]:
                css.append("opacity:.6")
            if st["it"]:
                css.append("font-style:italic")
            t = html.escape(buf)
            html_line.append('<span style="%s">%s</span>' % (";".join(css), t) if css else t)
            buf = ""

        pos = 0
        for m in SGR.finditer(line):
            seg = line[pos:m.start()]
            buf += "".join(glyph(c) for c in seg if c >= " " or c == "\t")
            pos = m.end()
            flush()
            nums = [int(x) if x else 0 for x in re.split("[;:]", m.group(1))] if m.group(1) else [0]
            i = 0
            while i < len(nums):
                n = nums[i]
                if n == 0:
                    st.update(fg=None, bg=None, b=False, dim=False, rev=False, it=False)
                elif n == 1:
                    st["b"] = True
                elif n == 2:
                    st["dim"] = True
                elif n == 3:
                    st["it"] = True
                elif n == 7:
                    st["rev"] = True
                elif n == 22:
                    st["b"] = st["dim"] = False
                elif n == 27:
                    st["rev"] = False
                elif n == 39:
                    st["fg"] = None
                elif n == 49:
                    st["bg"] = None
                elif n in (38, 48) and i + 1 < len(nums):
                    if nums[i + 1] == 2 and i + 4 < len(nums):
                        col = tuple(nums[i + 2:i + 5])
                        i += 4
                    elif nums[i + 1] == 5 and i + 2 < len(nums):
                        v = nums[i + 2]
                        col = (v, v, v)
                        i += 2
                    else:
                        col = None
                    if col:
                        st["fg" if n == 38 else "bg"] = col
                elif 30 <= n <= 37 or 90 <= n <= 97:
                    base = [(0, 0, 0), (252, 97, 141), (123, 216, 143), (252, 229, 102), (148, 138, 227), (171, 157, 242), (90, 212, 230), (200, 200, 200)]
                    st["fg"] = base[(n - 30) % 10 % 8]
                i += 1
        buf += "".join(glyph(c) for c in line[pos:] if c >= " ")
        flush()
        out.append("".join(html_line).rstrip() if html_line else "")
    return "\n".join(out)


SCREENS = [
    ("01-explorer", "Explorer", "Sizes and preview",
     "The popup opens on the focused pane's directory with a preview of the line under the cursor. Files show their size and folders their item count. Git markers sit in the gutter (M modified, ? untracked, ! ignored); symlinks show their target as virtual text.",
     ["/READ ⏎"]),
    ("02-edits", "Editing", "Pending edits",
     "Every change is just text. The gutter and header come from the real plan: + create, ~ rename, ⧉ copy, and two pending deletes counted in the header.",
     ["^p", "/todo ⏎ dd", "/README ⏎ cin INTRO ⎋", "/LIC ⏎ yyp A -APACHE ⎋", "/scripts ⏎ cin bin ⎋", "/run.log ⏎ dd", "gg O benches/render.rs ⎋", "o notes/ ⎋"]),
    ("03-move", "Move", "Cut here, paste there",
     "The symlink was cut in the root buffer and pasted into docs/, then renamed. The line keeps its hidden id, so the planner sees a move. Edits in the other directory stay pending.",
     ["/DESIGN ⏎ dd", "/docs ⏎ ⏎", "p", "cin architecture ⎋"]),
    ("12-keyhint", "g menu", "Press g, see what follows",
     "Prefix keys (g, space, Z) pop up a small menu of what can come next. Once files are yanked, it offers gm and gc to move or copy them to another folder; after a cut (dd) only gm.",
     ["/LICENSE ⏎", "V j y", "g"]),
    ("10-pick", "Move to…", "Fuzzy-pick the destination",
     "gm opens the finder in pick mode, rooted at the repository. Enter picks the folder; a file stands for its folder, and ^n turns the query into a new sub-folder. The move is staged as buffer edits and the review dialog follows.",
     ["m", "pars"]),
    ("04-confirm", "Confirm", "Review before anything touches disk",
     ":w collects edits from every directory into one plan, relative to their common parent. Deletes go to the Trash; the dialog says so on each line.",
     [":w ⏎"]),
    ("05-applied", "Applied", "After y",
     "Applied in three staged phases, then every buffer reloads. The deleted files are in the Trash and the new entries show up as untracked. gd switches from sizes to full details.",
     ["y", "-", "gd"]),
    ("06-finder", "Finder", "Fuzzy files",
     "Tab switches to the finder. A parallel walk that honours .gitignore feeds nucleo; matched characters are highlighted and the preview runs through bat.",
     ["⇥", "prsmod"]),
    ("07-grep", "Grep", "Live ripgrep",
     "Grep mode re-runs ripgrep as you type, debounced. The preview jumps to the hit line.",
     ["⇥ ⇥", "TODO"]),
    ("13-projects", "Projects", "Hop to a project",
     "The Projects tab lists every project under herdr-hop's roots: recent hops first (▎), then open workspaces (▶), then the rest. Rows carry the branch, changed files and agents; ◆ marks the project oil was opened in. Enter jumps to the workspace and tab open there, or creates one. Captured with a stub herdr in a sample home.",
     ["␣p"]),
    ("08-viewer", "Viewer", "Read it inside oil",
     "Enter shows the file in the oil window, highlighted and scrollable, opened at the grep hit. e edits it in $EDITOR inside the same popup; q goes back to where you came from.",
     ["⏎"]),
    ("09-help", "Help", "Everything on one sheet",
     "g? opens the full keymap inside the popup. Esc closes the explorer, and asks first when there are unsaved edits.",
     ["q", "esc", "^p", "gd gd", "g?"]),
]


def screens():
    res = []
    for key, tab, title, cap, keys in SCREENS:
        raw = open(os.path.join(SHOTS, key + ".ansi"), encoding="utf-8", errors="replace").read()
        plain = re.sub(r"\x1b\[[0-9;:]*m", "", raw).replace("\r", "").split("\n")
        cols = max(len(l.rstrip()) for l in plain)
        rows = min(24, len([l for l in plain]))
        res.append({"key": key, "tab": tab, "title": title, "caption": cap, "keys": keys, "cols": cols, "rows": rows, "html": ansi_to_html(raw, 24)})
    return res


ORDER = ["main.rs", "app.rs", "buffer.rs", "vim.rs", "plan.rs", "apply.rs", "finder.rs", "projects.rs", "preview.rs", "open.rs", "launch.rs", "entry.rs", "config.rs", "ui.rs", "theme.rs"]
ROLE = {
    "main.rs": "entry point, CLI flags, terminal setup",
    "app.rs": "state, event loop, save/quit flow, finder keys",
    "buffer.rs": "lines with hidden ids, undo/redo, text primitives",
    "vim.rs": "modal editing: counts, operators, motions, text objects",
    "plan.rs": "buffer diff → create/move/copy/delete + validation",
    "apply.rs": "staged execution, trash, cross-device fallback",
    "finder.rs": "nucleo matcher, ignore walker, ripgrep, recent, finder tabs",
    "projects.rs": "Projects tab: index, recent hops, herdr workspaces, hop",
    "preview.rs": "bat preview, ANSI parser, dir listings",
    "open.rs": "editor argv, split/tab/origin/popup hand-off",
    "launch.rs": "herdr CLI: focused pane, popup open, pane ids",
    "entry.rs": "listing, id store, icons, git status, sorting",
    "config.rs": "config.toml options",
    "ui.rs": "rendering of every view and overlay",
    "theme.rs": "colours from herdr [theme.custom]",
}


def sources():
    files = []
    for name in ORDER:
        p = os.path.join(SRC, "src", name)
        txt = open(p).read()
        files.append({"name": "src/" + name, "role": ROLE[name], "lines": txt.count("\n"), "code": txt, "lang": "rust"})
    for name, lang, role in [("herdr-plugin.toml", "ini", "plugin manifest"), ("config.example.toml", "ini", "documented options"), ("README.md", "markdown", "user docs"), ("Cargo.toml", "ini", "crate + deps"), ("scripts/build.sh", "bash", "build hook")]:
        txt = open(os.path.join(SRC, name)).read()
        files.append({"name": name, "role": role, "lines": txt.count("\n"), "code": txt, "lang": lang})
    return files


def tests():
    import subprocess
    raw = subprocess.run(["cargo", "test", "--release"], cwd=SRC, capture_output=True, text=True).stdout
    names = re.findall(r"^test (\S+) \.\.\. ok", raw, re.M)
    groups = {}
    for n in names:
        mod, _, t = n.partition("::tests::")
        groups.setdefault(mod, []).append(t)
    return {k: sorted(v) for k, v in sorted(groups.items())}


def main():
    tpl = open(os.path.join(SP, "page.tpl.html")).read()
    data = {"screens": screens(), "sources": sources(), "tests": tests()}
    blob = json.dumps(data).replace("</", "<\\/")
    page = tpl.replace("/*__DATA__*/null", blob)
    page = page.replace("__TESTCOUNT__", str(sum(len(v) for v in data["tests"].values())))
    page = page.replace("__LOC__", "{:,}".format(sum(f["lines"] for f in data["sources"] if f["lang"] == "rust")))
    page = page.replace("__HERO__", data["screens"][1]["html"]).replace("__HEROCOLS__", str(data["screens"][1]["cols"]))
    head = ('<!doctype html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
            '<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">\n')
    # The template starts with <title>/<style>; close head before the first page element.
    i = page.index('<div class="wrap">')
    page = head + page[:i] + "</head>\n<body>\n" + page[i:] + "\n</body>\n</html>\n"
    out = os.path.join(SRC, "docs", "index.html")
    open(out, "w").write(page)
    print(out, len(page) // 1024, "KB")


if __name__ == "__main__":
    main()
