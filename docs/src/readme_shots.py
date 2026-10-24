"""Render README screenshots (docs/img/*.png) from the ANSI captures in shots/.

    python docs/src/readme_shots.py

Uses headless Chrome and a locally installed Nerd Font, so icons show as in
the terminal. Same captures as the project page (build_page.py).
"""
import os
import subprocess
import tempfile

import build_page as bp

CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
FONT = '"JetBrainsMono Nerd Font Mono", "JetBrainsMono NFM", Menlo, monospace'
OUT = os.path.join(bp.SRC, "docs", "img")
SHOTS = [("02-edits", "explorer.png", "Oil — ~/atlas"), ("13-projects", "projects.png", "Find — Projects")]
SIZE, LINE, CHAR = 14, 19, 8.43  # px; JetBrains Mono advance is 0.602em

bp.glyph = lambda ch: ch  # keep Nerd Font icons


def page(body, title, cols, rows):
    return f"""<!doctype html><meta charset="utf-8"><style>
html,body{{margin:0;background:#16181a}}
.win{{margin:18px;border-radius:10px;overflow:hidden;background:#222222;box-shadow:0 10px 30px rgba(0,0,0,.45);border:1px solid #3a3f44;width:{cols * CHAR + 32:.0f}px}}
.bar{{height:30px;display:flex;align-items:center;gap:8px;padding:0 12px;background:#2a2a2a;border-bottom:1px solid #3a3f44;font:600 12px -apple-system,Helvetica,sans-serif;color:#9aa0a6}}
.bar i{{width:11px;height:11px;border-radius:50%;display:inline-block}}
.bar span{{margin-left:8px}}
pre{{margin:0;padding:12px 16px 14px;font:{SIZE}px/{LINE}px {FONT};color:rgb(247,241,255);white-space:pre;height:{rows * LINE}px}}
</style><div class="win"><div class="bar"><i style="background:#fc618d"></i><i style="background:#fce566"></i><i style="background:#7bd88f"></i><span>{title}</span></div><pre>{body}</pre></div>"""


def main():
    os.makedirs(OUT, exist_ok=True)
    for key, name, title in SHOTS:
        raw = open(os.path.join(bp.SHOTS, key + ".ansi"), encoding="utf-8", errors="replace").read()
        lines = raw.replace("\r", "").split("\n")
        plain = [bp.re.sub(r"\x1b\[[0-9;:]*m", "", l) for l in lines]
        filled = [i for i, l in enumerate(plain) if l.strip()]
        # the content, one blank row, then the status bar (tall captures have a gap between)
        top = [i for i in filled[:-1] if i < 22] or filled[:1]
        keep = list(range(top[-1] + 1)) + [top[-1] + 1, filled[-1]]
        rows = len(keep)
        body = bp.ansi_to_html("\n".join(lines[i] for i in keep), rows)
        cols = max(len(l) for l in plain)
        w, h = int(cols * CHAR + 32 + 36 + 2), int(rows * LINE + 26 + 30 + 36 + 2)
        with tempfile.NamedTemporaryFile("w", suffix=".html", delete=False) as f:
            f.write(page(body, title, cols, rows))
        out = os.path.join(OUT, name)
        subprocess.run([CHROME, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=2",
                        f"--window-size={w},{h}", f"--screenshot={out}", "file://" + f.name],
                       check=True, capture_output=True)
        os.unlink(f.name)
        print(out, w, h)


if __name__ == "__main__":
    main()
