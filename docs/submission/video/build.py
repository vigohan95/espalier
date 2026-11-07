#!/usr/bin/env python3
"""Builds the Espalier demo video from the recorded testnet run. No screen recording.

    python docs/submission/video/build.py                 # silent master, plus a small copy
    python docs/submission/video/build.py --voice         # also a copy with a synthetic voice
    python docs/submission/video/build.py --plan          # print the timing and stop
    python docs/submission/video/build.py --frames 12,40  # write stills at these seconds to tmp/

The video has to work with the sound off, so every sentence is on screen, and timing comes from reading
speed, not from a voice. `--voice` lays edge-tts narration over the same frames as a second file.

What keeps it honest:
  - Every number in a caption is a field filled in from docs/evidence/, never typed in storyboard.json.
  - A caption can name `proof`: text that has to be a passed check in docs/evidence/swarm-sim-testnet.json.
    If it is not there, the build stops.
  - The tree, its prices, bids, hashes and meters are drawn from the same run. The meters are replayed from
    the run's draws, and the build stops if they do not end on what the vault reported.
  - The terminal scene is docs/evidence/verify-market.log, and the page scene is site/index.html as it renders.

Requires ffmpeg and ffprobe on PATH, Pillow, Chrome for the page scene (CHROME), edge-tts for --voice, and the
fonts below (FONT_DIR).
"""
import argparse
import asyncio
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys

from PIL import Image, ImageDraw, ImageFont

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[2]
EV = ROOT / "docs" / "evidence"
OUT, TMP = HERE / "out", HERE / "tmp"
FONT_DIR = pathlib.Path(os.environ.get("FONT_DIR", "C:/Windows/Fonts"))
CHROME = os.environ.get("CHROME", "C:/Program Files/Google/Chrome/Application/chrome.exe")

W, H, FPS, SS = 1920, 1080, 24, 2
BG, CARD, INK, SOFT, LINE = "#121a15", "#1a261e", "#e6ecdf", "#9fb0a3", "#34463b"
LEAF, BARK, CUT, CUTBG, AMBER, BLUE = "#7fc48c", "#c9a66b", "#e58a76", "#3b211c", "#e0b34a", "#7db4e6"
CAPTION_TOP = 906
WORDS_PER_S, MIN_CAPTION_S = 2.9, 2.7
FORBIDDEN = re.compile(r"ed25519:|near-credentials|secret_key|private_key", re.I)
MILLI = 10**21

_fonts = {}


def font(name, size):
    key = (name, size)
    if key not in _fonts:
        _fonts[key] = ImageFont.truetype(str(FONT_DIR / name), size)
    return _fonts[key]


SERIF, SANS, SANSB, MONO, MONOB = "georgiab.ttf", "segoeui.ttf", "segoeuib.ttf", "consola.ttf", "consolab.ttf"


def rgb(c):
    c = c.lstrip("#")
    return tuple(int(c[i:i + 2], 16) for i in (0, 2, 4))


def mix(a, b, p):
    a, b = rgb(a) if isinstance(a, str) else a, rgb(b) if isinstance(b, str) else b
    return tuple(int(x + (y - x) * p) for x, y in zip(a, b))


def ease(x):
    x = min(1.0, max(0.0, x))
    return x * x * (3 - 2 * x)


# ---- What the video is allowed to say ---------------------------------------------------------------


def load_run():
    run = json.loads((EV / "swarm-sim-testnet.json").read_text(encoding="utf-8"))
    report = json.loads((EV / "sandbox-report.json").read_text(encoding="utf-8"))
    prop = json.loads((EV / "market-property.json").read_text(encoding="utf-8"))
    snap = (ROOT / "site" / "snapshot.js").read_text(encoding="utf-8")
    snap = json.loads(snap[snap.index("window.ESPALIER_SNAPSHOT = ") + 27:].rstrip().rstrip(";"))
    if not (run["passed"] and run["matches_model"] and report["passed"] == report["total"] and not prop["broken"]):
        sys.exit("the evidence files do not all pass, so there is nothing to show")
    if snap["market"] != run["market"]:
        sys.exit("site/snapshot.js is not from the testnet run")
    tasks = [t for k, v in snap["calls"].items() if "|get_tasks|" in k for t in v]
    commitments = {t["id"]: {b["worker"].split(".")[0]: b["commitment"] for b in t["bids"]} for t in tasks}
    g, t = run["grants"], run["tasks"]
    hal_revoke = next(x for x in run["txs"] if x["what"] == f"vault.revoke {g['hal']['grant']}")
    paid = sum(x["paid_mnear"] for x in t.values())
    back = sum(x["refunded_mnear"] for x in t.values())
    below_hal = [n for n, v in g.items() if n != "hal" and is_below(g, v, g["hal"]["grant"])]
    facts = {
        "root_cap": g["gc"]["cap_mnear"], "root_near": g["gc"]["cap_mnear"] // 1000, "root_spent": g["gc"]["spent_mnear"], "paid": paid, "back": back,
        "auctions": len(t), "grants": len(g) - 1, "txs": run["transactions"], "checks": len(run["checks"]),
        "scenarios": report["total"], "assertions": report["assertions"],
        "market_scenarios": sum(1 for s in report["scenarios"] if s["group"] == "market"),
        "prop_calls": prop["transactions"], "prop_checks": f"{prop['invariant_checks']:,}",
        "below_hal": len(below_hal), "revoke_tx": hal_revoke["hash"], "market": run["market"], "vault": run["vault"],
        "vault_sha": report["wasm_sha256"][:12],
    }
    if paid + back != facts["root_spent"]:
        sys.exit(f"paid {paid} + back {back} is not what the root was charged, {facts['root_spent']}")
    return run, facts, commitments


def is_below(grants, node, top):
    by_id = {v["grant"]: v for v in grants.values()}
    cur = node
    while cur["parent"] is not None:
        if cur["parent"] == top:
            return True
        cur = by_id[cur["parent"]]
    return False


def check_proofs(sb, run):
    passed = {c["claim"] for c in run["checks"] if c["pass"]}
    for s in sb["scenes"]:
        for c in s.get("captions", []):
            for p in c.get("proof", []):
                if not any(p in claim for claim in passed):
                    sys.exit(f"caption {c['id']}: no passed check in the testnet run says {p!r}")


def plan(sb, facts):
    """Fills in numbers, and works out when each caption starts from how long it takes to read."""
    t, marks = 0.0, {}
    for s in sb["scenes"]:
        s["start"] = t
        for c in s["captions"]:
            c["text"] = c["text"].format(**facts)
            words = len(c["text"].split())
            c["start"] = t
            c["len"] = max(MIN_CAPTION_S, words / WORDS_PER_S + 0.4) + c.get("extra", 0.0)
            marks[c["id"]] = t
            t += c["len"]
        s["end"] = t
    return t, marks


# ---- Drawing helpers ---------------------------------------------------------------------------------


def wrap(text, f, width):
    lines, cur = [], ""
    for word in text.split():
        trial = f"{cur} {word}".strip()
        if f.getlength(trial) <= width:
            cur = trial
        else:
            lines.append(cur)
            cur = word
    return lines + [cur]


def caption(img, text):
    d = ImageDraw.Draw(img)
    f = font(SANS, 43)
    lines = wrap(text, f, W - 220)
    d.rectangle((0, CAPTION_TOP, W, H), fill=mix(BG, "#000000", 0.45))
    d.line((0, CAPTION_TOP, W, CAPTION_TOP), fill=LINE, width=2)
    y = CAPTION_TOP + (H - CAPTION_TOP) / 2 - (len(lines) - 1) * 29
    for line in lines:
        d.text((W / 2, y), line, font=f, fill=INK, anchor="mm")
        y += 58


def header(img, text):
    ImageDraw.Draw(img).text((40, 26), text, font=font(SANS, 25), fill=SOFT)


def pill(d, x, y, text, f, fg, bg, scale=1, anchor="mm", alpha=1.0, pad=(14, 7)):
    """A small label centred on (x, y)."""
    w = f.getlength(text) + 2 * pad[0] * scale
    h = f.size + 2 * pad[1] * scale
    x0 = x - w / 2 if anchor == "mm" else x
    d.rounded_rectangle((x0, y - h / 2, x0 + w, y + h / 2), 9 * scale, fill=mix(BG, bg, alpha), outline=mix(BG, fg, alpha), width=2 * scale)
    d.text((x0 + w / 2, y), text, font=f, fill=mix(BG, fg, alpha), anchor="mm")


# ---- The tree ----------------------------------------------------------------------------------------

POS = {
    "gc": (960, 128), "ada": (350, 318), "eve": (960, 318), "hal": (1500, 318),
    "kai": (200, 526), "ned": (500, 526), "quin": (810, 526), "sam": (1110, 526), "uma": (1400, 526), "wes": (1680, 526),
    "cy": (200, 736), "fay": (810, 736), "ida": (1400, 736),
}
BOX_W, BOX_H = 232, 108
LABEL = {"gc": "general contractor"}


class Tree:
    """The state of the tree at any moment, as a pure function of time."""

    def __init__(self, run, marks):
        self.g = run["grants"]
        self.t = run["tasks"]
        self.by_grant = {v["grant"]: n for n, v in self.g.items()}
        self.task_of = {v["winner"]: k for k, v in self.t.items() if v["winner"] in self.g}
        m = marks
        self.born = {"gc": m["c1"]}
        self.born["ada"] = m["p1"] + 0.5
        self.born["eve"], self.born["hal"] = m["p2"] + 0.2, m["p2"] + 0.9
        for i, n in enumerate(("kai", "ned", "quin", "sam", "uma", "wes")):
            self.born[n] = m["l1"] + 0.4 + 0.42 * i
        for i, n in enumerate(("cy", "fay", "ida")):
            self.born[n] = m["l3"] + 0.3 + 0.55 * i
        cut_at = m["r1"] + 1.3
        self.cut = {n: cut_at for n in ("hal", "uma", "wes", "ida")}
        ended = m["m2b"] + 0.7
        self.cut.update({n: ended for n in ("eve", "quin", "sam", "fay", "ned", "cy")})
        self.hijacked_at, self.flash_at = m["h1"], cut_at
        # The run's draws, replayed. Each is charged to the grant and to every level above it.
        self.draws = [(m["m1"] + 1.2, "fay", 30), (m["m2"] + 0.2, "kai", 70), (m["m2"] + 2.6, "ada", 160), (m["m2b"] + 0.2, "quin", 70), (m["m3"] + 0.3, "hal", 100)]
        for name, v in self.g.items():
            if self.spent(name, 1e9) != v["spent_mnear"]:
                sys.exit(f"replayed draws give {name} {self.spent(name, 1e9)}, the vault says {v['spent_mnear']}")
        self.m = m

    def lineage(self, name):
        out = [name]
        while self.g[out[-1]]["parent"] is not None:
            out.append(self.by_grant[self.g[out[-1]]["parent"]])
        return out

    def spent(self, name, t):
        total = 0.0
        for at, who, amount in self.draws:
            if name in self.lineage(who):
                total += amount * ease((t - at) / 0.8)
        return total

    def price(self, name):
        return self.t[self.task_of[name]]["price_mnear"]

    def state(self, t):
        nodes = {}
        for n, born in self.born.items():
            p = ease((t - born) / 0.55)
            if p <= 0:
                continue
            cut = ease((t - self.cut[n]) / 0.3) if n in self.cut else 0.0
            nodes[n] = (round(p, 2), round(cut, 2), round(self.spent(n, t), 1))
        return nodes


def draw_tree(tree, t, tags):
    S = SS
    img = Image.new("RGB", (W * S, H * S), BG)
    d = ImageDraw.Draw(img)
    state = tree.state(t)
    fn, fs, fp = font(SERIF, 30 * S), font(MONO, 22 * S), font(MONOB, 25 * S)

    def box(n):
        x, y = POS[n]
        return (x - BOX_W / 2) * S, (y - BOX_H / 2) * S, (x + BOX_W / 2) * S, (y + BOX_H / 2) * S

    for n, (p, cut, _) in state.items():  # edges first
        if n == "gc":
            continue
        parent = tree.by_grant[tree.g[n]["parent"]]
        x0, y0 = POS[parent][0] * S, (POS[parent][1] + BOX_H / 2) * S
        x1, y1 = POS[n][0] * S, (POS[n][1] - BOX_H / 2) * S
        ym = (y0 + y1) / 2
        color = mix(LINE, CUT, cut) if cut else mix(LINE, LEAF, 0.55)
        pts = [(x0, y0), (x0, ym), (x1, ym), (x1, y1)]
        total = abs(ym - y0) + abs(x1 - x0) + abs(y1 - ym)
        left = total * p
        for a, b in zip(pts, pts[1:]):
            seg = abs(b[0] - a[0]) + abs(b[1] - a[1])
            if seg == 0:
                continue
            k = min(1.0, left / seg)
            d.line((a, (a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k)), fill=color, width=4 * S)
            left -= seg
            if left <= 0:
                break
    for n, (p, cut, spent) in state.items():
        a = ease((p - 0.45) / 0.55)
        if a <= 0:
            continue
        x0, y0, x1, y1 = box(n)
        hij = ease((t - tree.hijacked_at) / 0.4) if n == "hal" and not cut else 0.0
        edge = mix(mix(LEAF, AMBER, hij), CUT, cut)
        fill = mix(CARD, CUTBG, cut)
        d.rounded_rectangle((x0, y0, x1, y1), 12 * S, fill=mix(BG, fill, a), outline=mix(BG, edge, a), width=3 * S)
        cx = (x0 + x1) / 2
        d.text((cx, y0 + 33 * S), LABEL.get(n, n) if n != "gc" else "contractor", font=fn, fill=mix(BG, INK, a), anchor="mm")
        cap = tree.g[n]["cap_mnear"]
        d.text((cx, y0 + 68 * S), f"grant {tree.g[n]['grant']}", font=fs, fill=mix(BG, SOFT, a), anchor="mm")
        bx0, bx1, by = x0 + 18 * S, x1 - 18 * S, y1 - 17 * S
        d.rounded_rectangle((bx0, by, bx1, by + 8 * S), 4 * S, fill=mix(BG, LINE, a))
        if spent > 0:
            d.rounded_rectangle((bx0, by, bx0 + (bx1 - bx0) * min(1.0, spent / cap), by + 8 * S), 4 * S, fill=mix(BG, BARK, a))
        if n != "gc":
            pill(d, cx, y0 - 2 * S, str(tree.price(n)), fp, mix(mix(LEAF, CUT, cut), BG, 0.0), CARD, S, alpha=a, pad=(11, 2))
    for tag in tags:
        p = ease((t - tag["at"]) / 0.25)
        if p > 0 and t < tag.get("until", 1e9):
            tag["draw"](d, p)
    if tree.flash_at <= t < tree.flash_at + 0.5:  # one flash over the whole branch, in the same instant
        q = 1 - (t - tree.flash_at) / 0.5
        for n in ("hal", "uma", "wes", "ida"):
            x0, y0, x1, y1 = box(n)
            g = 12 * S
            d.rounded_rectangle((x0 - g, y0 - g, x1 + g, y1 + g), 16 * S, outline=mix(BG, CUT, q), width=5 * S)
    out = img.resize((W, H), Image.LANCZOS)
    # The root's meter, the number everything else is charged to.
    dd = ImageDraw.Draw(out)
    spent, cap = tree.spent("gc", t), tree.g["gc"]["cap_mnear"]
    dd.text((W - 40, 26), f"charged to the root: {spent:.0f} of {cap} mNEAR", font=font(MONO, 26), fill=INK, anchor="ra")
    return out, (tuple(sorted(state.items())), tuple(ease((t - x["at"]) / 0.25) > 0 and t < x.get("until", 1e9) and round(ease((t - x["at"]) / 0.25), 2) for x in tags), round(spent, 1), tree.flash_at <= t < tree.flash_at + 0.5 and round(t, 2))


def tags_for(tree, m, tx):
    S = SS
    f = font(MONOB, 22 * S)

    def red_pill(x, y, text):
        return lambda d, p: pill(d, x * S, y * S, text, f, CUT, CUTBG, S, alpha=p)

    def outsider(d, p):
        x, y = 1786, 150
        hx, hy = POS["hal"][0] + 60, POS["hal"][1] - BOX_H / 2
        d.rounded_rectangle(((x - 100) * S, (y - 42) * S, (x + 100) * S, (y + 42) * S), 12 * S, fill=mix(BG, CUTBG, p), outline=mix(BG, CUT, p), width=3 * S)
        d.text((x * S, y * S), "outsider", font=font(SERIF, 30 * S), fill=mix(BG, INK, p), anchor="mm")
        steps = 11
        for i in range(0, steps, 2):
            a = (hx + (x - 60 - hx) * i / steps, hy + (y + 42 - hy) * i / steps)
            b = (hx + (x - 60 - hx) * (i + 1) / steps, hy + (y + 42 - hy) * (i + 1) / steps)
            d.line(((a[0] * S, a[1] * S), (b[0] * S, b[1] * S)), fill=mix(BG, CUT, p), width=4 * S)
        mx, my = (hx + x - 60) / 2, (hy + y + 42) / 2
        r = 15
        for sx in (1, -1):
            d.line((((mx - r * sx) * S, (my - r) * S), ((mx + r * sx) * S, (my + r) * S)), fill=mix(BG, CUT, p), width=6 * S)

    return [
        {"at": m["l2"] + 1.0, "until": m["l3"], "draw": red_pill(1230, POS["eve"][1], "OVERCOMMITTED")},
        {"at": m["h1"] + 0.8, "until": m["r1"] + 1.3, "draw": outsider},
        {"at": m["h1"] + 1.6, "until": m["h2"], "draw": red_pill(1765, POS["hal"][1], "RECIPIENT_NOT_ALLOWED")},
        {"at": m["h2"] + 0.7, "until": m["r1"], "draw": red_pill(1765, POS["hal"][1], "ATT_RECIPIENTS")},
        {"at": m["r2"], "until": m["r3"], "draw": red_pill(1400, 838, f"4 grants, 1 transaction: {tx[:12]}...")},
        {"at": m["r3"], "until": m["m1"], "draw": red_pill(1400, 838, "their 4 tasks: void, bonds returned")},
        {"at": m["m2"] + 1.2, "until": m["m2b"], "draw": red_pill(POS["ada"][0] + 170, POS["ada"][1], "CAP")},
    ]


# ---- Other scenes ------------------------------------------------------------------------------------


def draw_title(lines, sub):
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)
    y = 470
    for line in lines:
        d.text((W / 2, y), line, font=font(SERIF, 78), fill=INK, anchor="mm")
        y += 118
    d.text((W / 2, y + 26), sub, font=font(SANS, 36), fill=SOFT, anchor="mm")
    return img


def draw_auction(run, commits, t, m):
    """Task 1, from the run: hashes on chain, then the bids as they opened."""
    task = run["tasks"]["api"]
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)
    d.text((W / 2, 120), f"Task {task['id']}: up to 300 mNEAR. Four workers bid.", font=font(SERIF, 46), fill=INK, anchor="mm")
    rows = list(task["bids_mnear"].items())
    order = commits[task["id"]]
    open_at = m["a2"] + 0.3
    y = 240
    for i, (who, bid) in enumerate(rows):
        reveal = ease((t - (open_at + 0.35 * i)) / 0.3) if bid is not None else 0.0
        silent = bid is None and t >= open_at + 1.6
        win = who == task["winner"] and t >= m["a3"] + 0.2
        sets = bid == task["price_mnear"] and t >= m["a3"] + 1.2
        edge = CUT if silent else LEAF if win else AMBER if sets else LINE
        d.rounded_rectangle((280, y, 1640, y + 124), 12, fill=CUTBG if silent else CARD, outline=edge, width=3)
        d.text((330, y + 62), who, font=font(SERIF, 42), fill=INK, anchor="lm")
        h = "0x" + order[who][:30] + "..."
        d.text((560, y + 62), h, font=font(MONO, 32), fill=mix(CARD, SOFT, 1 - reveal), anchor="lm")
        if bid is not None and reveal > 0:
            d.text((560, y + 62), f"{bid} mNEAR", font=font(MONOB, 44), fill=mix(CARD, INK, reveal), anchor="lm")
        note = "never revealed: bond lost" if silent else f"wins, paid {task['price_mnear']}" if win else "sets the price" if sets else ""
        d.text((1600, y + 62), note, font=font(SANSB, 34), fill=CUT if silent else LEAF if win else AMBER, anchor="rm")
        y += 148
    return img, (round(t - open_at, 1) if open_at <= t < open_at + 2.2 else t >= open_at, t >= m["a3"] + 0.2, t >= m["a3"] + 1.2)


def shoot_page():
    """The real page, from the snapshot of the testnet run, at twice the pixels."""
    png = TMP / "page.png"
    url = (ROOT / "site" / "index.html").as_uri() + "?source=snapshot"
    subprocess.run(
        [CHROME, "--headless=new", "--disable-gpu", "--no-first-run", f"--user-data-dir={TMP / 'chrome'}", "--hide-scrollbars", "--force-device-scale-factor=2",
         "--force-dark-mode", "--enable-features=WebContentsForceDark:inversion_method/cielab_based", "--window-size=1180,2300", "--virtual-time-budget=6000", f"--screenshot={png}", url],
        check=True, timeout=120, capture_output=True,
    )
    return Image.open(png).convert("RGB")


def draw_page(page, u):
    """A slow pan down the page. `u` runs from 0 to 1."""
    width = 1300
    scale = width / page.width
    view = int(860 / scale)
    y = int((page.height - view) * 0.42 * ease(u))
    crop = page.crop((0, y, page.width, y + view)).resize((width, 860), Image.LANCZOS)
    img = Image.new("RGB", (W, H), BG)
    img.paste(crop, ((W - width) // 2, 40))
    return img


def verify_text():
    text = (EV / "verify-market.log").read_text(encoding="utf-8")
    if FORBIDDEN.search(text):
        sys.exit("verify-market.log holds key material")
    return [line.rstrip() for line in text.split("\n") if line.strip()]


def draw_verify(lines, u):
    img = Image.new("RGB", (W, H), "#0d1210")
    d = ImageDraw.Draw(img)
    f, fb = font(MONO, 25), font(MONOB, 25)
    y = 34
    d.text((48, y), "$ python3 scripts/verify-market.py", font=fb, fill=LEAF)
    y += 46
    shown = int(len(lines) * min(1.0, u / 0.45)) + 1
    for line in lines[:shown]:
        keep = line.startswith("  ok") and ("transactions found" in line or "recomputed" in line) or "all checks passed" in line
        if len(line) > 118:
            line = line[:115] + "..."
        if keep:
            d.rectangle((40, y - 4, W - 40, y + 32), fill=mix("#0d1210", LEAF, 0.16))
        d.text((48, y), line, font=fb if keep else f, fill=INK if keep else SOFT)
        y += 37
    return img, shown


def draw_end(card, facts, repo):
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)
    x, y = 170, 118
    d.text((x, y), "Espalier", font=font(SERIF, 88), fill=INK)
    y += 122
    for line in card["lines"]:
        d.text((x, y), line, font=font(SANS, 38), fill=SOFT)
        y += 54
    y += 34
    for line in card["facts"]:
        d.text((x, y), line.format(**facts), font=font(MONO, 34), fill=INK)
        y += 52
    y += 34
    d.text((x, y), repo, font=font(MONOB, 44), fill=BLUE)
    y += 78
    d.text((x, y), card["status"], font=font(SANSB, 38), fill=AMBER)
    return img


# ---- Assembly ----------------------------------------------------------------------------------------


class Film:
    def __init__(self, sb, run, facts, commits, marks, total, repo):
        self.sb, self.run, self.facts, self.commits, self.m, self.total = sb, run, facts, commits, marks, total
        self.tree = Tree(run, marks)
        self.tags = tags_for(self.tree, marks, facts["revoke_tx"])
        self.page = shoot_page()
        self.log = verify_text()
        self.title = draw_title(sb["title_card"], sb["title_sub"])
        self.end = draw_end(sb["end_card"], facts, repo)
        self.scenes = sb["scenes"]
        self.tree_header = sb["tree_header"].format(**facts)
        self.memo = (None, None)

    def frame(self, t):
        s = next((x for x in self.scenes if x["start"] <= t < x["end"]), self.scenes[-1])
        c = next((x for x in s["captions"] if x["start"] <= t < x["start"] + x["len"]), s["captions"][-1])
        kind = s["kind"]
        if kind == "tree":
            base, key = draw_tree(self.tree, t, self.tags)
            key = ("tree", key)
            head = self.tree_header
        elif kind == "auction":
            base, key = draw_auction(self.run, self.commits, t, self.m)
            key = ("auction", key)
            head = self.tree_header
        elif kind == "page":
            u = (t - s["start"]) / (s["end"] - s["start"])
            base, key, head = draw_page(self.page, u), ("page", round(u, 3)), None
        elif kind == "verify":
            u = (t - s["start"]) / (s["end"] - s["start"])
            base, shown = draw_verify(self.log, u)
            key, head = ("verify", shown), None
        elif kind == "title":
            base, key, head = self.title.copy(), ("title",), None
        else:
            base, key, head = self.end.copy(), ("end",), None
        key = (key, c["id"])
        if key == self.memo[0]:
            return self.memo[1]
        if head:
            header(base, head)
        if c["text"]:
            caption(base, c["text"])
        raw = base.tobytes()
        self.memo = (key, raw)
        return raw


def render(film, path):
    cmd = ["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}", "-r", str(FPS), "-i", "-",
           "-vf", "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p", "-c:v", "libx264", "-preset", "medium", "-crf", "18",
           "-g", str(FPS * 2), "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-movflags", "+faststart", str(path)]
    ff = subprocess.Popen(cmd, stdin=subprocess.PIPE)
    for n in range(int(round(film.total * FPS))):
        ff.stdin.write(film.frame(n / FPS))
    ff.stdin.close()
    if ff.wait(timeout=300) != 0:
        sys.exit("ffmpeg failed while rendering")


def duration(path):
    return float(subprocess.check_output(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)], timeout=30).decode().strip())


async def speak(text, voice, rate, mp3):
    import edge_tts
    await edge_tts.Communicate(text, voice, rate=rate).save(str(mp3))


def voice_track(sb, total, wav):
    """Each caption spoken at its own start."""
    sounds = sb["say"]
    cmd, parts, n = ["ffmpeg", "-y", "-loglevel", "error"], [], 0
    for s in sb["scenes"]:
        for c in s["captions"]:
            if not c["text"]:
                continue
            text = c["text"]
            for a, b in sounds.items():
                text = text.replace(a, b)
            key = hashlib.sha256(f"{sb['voice']}|{sb['rate']}|{text}".encode()).hexdigest()[:16]
            mp3 = TMP / f"{key}.mp3"
            if not mp3.exists():
                asyncio.run(speak(text, sb["voice"], sb["rate"], mp3))
            # A clip longer than its caption is sped up a little, so that it never runs into the next one.
            speed = max(1.0, duration(mp3) / (c["len"] - 0.2))
            if speed > 1.15:
                print(f"voice is {speed:.2f}x on caption {c['id']}")
            cmd += ["-i", str(mp3)]
            parts.append(f"[{n}:a]atempo={speed:.3f},aresample=48000,adelay={int((c['start'] + 0.1) * 1000)}:all=1[a{n}]")
            n += 1
    labels = "".join(f"[a{i}]" for i in range(n))
    graph = ";".join(parts) + f";{labels}amix=inputs={n}:normalize=0:duration=longest,apad,atrim=0:{total:.3f},alimiter=limit=0.89[out]"
    subprocess.run(cmd + ["-filter_complex", graph, "-map", "[out]", "-ac", "2", "-ar", "48000", str(wav)], check=True, timeout=180)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--plan", action="store_true")
    ap.add_argument("--voice", action="store_true")
    ap.add_argument("--frames", help="comma-separated seconds: write stills to tmp/ and stop")
    ap.add_argument("--repo-url")
    args = ap.parse_args()
    sb = json.loads((HERE / "storyboard.json").read_text(encoding="utf-8"))
    OUT.mkdir(exist_ok=True)
    TMP.mkdir(exist_ok=True)

    run, facts, commits = load_run()
    check_proofs(sb, run)
    total, marks = plan(sb, facts)
    rows = [{"id": c["id"], "start": round(c["start"], 2), "seconds": round(c["len"], 2), "text": c["text"]} for s in sb["scenes"] for c in s["captions"]]
    (OUT / "timing.json").write_text(json.dumps({"total": round(total, 2), "facts": facts, "captions": rows}, indent=1), encoding="utf-8")
    for r in rows:
        print(f"{r['start']:7.2f} {r['seconds']:5.2f}  {r['id']:6} {r['text']}")
    print(f"total {total:.2f} s")
    if args.plan:
        return

    film = Film(sb, run, facts, commits, marks, total, args.repo_url or sb["repo_url"])
    if args.frames:
        for sec in args.frames.split(","):
            raw = film.frame(float(sec))
            Image.frombytes("RGB", (W, H), raw).save(TMP / f"still-{float(sec):06.1f}.png")
        return
    final, small = OUT / sb["output"], OUT / sb["output_small"]
    render(film, final)
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", str(final), "-c:v", "libx264", "-preset", "slow", "-crf", "24", "-pix_fmt", "yuv420p",
                    "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-movflags", "+faststart", str(small)], check=True, timeout=400)
    outs = [final, small]
    if args.voice:
        wav, voiced = TMP / "voice.wav", OUT / sb["output_voiced"]
        voice_track(sb, total, wav)
        subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", str(small), "-i", str(wav), "-c:v", "copy", "-c:a", "aac", "-b:a", "128k", "-movflags", "+faststart", "-t", f"{total:.3f}", str(voiced)], check=True, timeout=120)
        outs.append(voiced)
    for p in outs:
        print(f"{p}  {p.stat().st_size / 1e6:.1f} MB  {duration(p):.2f}s")


if __name__ == "__main__":
    main()
