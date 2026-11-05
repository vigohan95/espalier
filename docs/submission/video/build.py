#!/usr/bin/env python3
"""Builds the Espalier demo video from real terminal output. No browser, no screen recording.

    python docs/submission/video/build.py                 # capture, voice, render, mux, small copy
    python docs/submission/video/build.py --plan          # capture and voice only, print the timing
    python docs/submission/video/build.py --repo-url URL  # end card shows URL instead of storyboard repo_url

Pipeline:
  1. captures: run the repository's commands and read its stored evidence files. The text goes to
     captured/*.txt, unedited. Nothing on a terminal screen comes from anywhere else, and a highlight is
     a substring of that text: if the text is not there, the build stops.
  2. edge-tts speaks each scene and returns word timings. A beat in storyboard.json names the phrase it
     starts on, so highlights and scrolling follow the voice rather than a hand-set clock.
  3. PIL draws each frame (scrolling, highlight boxes) and pipes it to ffmpeg, H.264 at 1080p24.
  4. ffmpeg places each narration at its scene time and muxes AAC. A copy for upload tools goes next to it.

The build refuses any captured text that holds a key: docs/evidence/attack-demo.log line 4 has a public key
fragment, so the storyboard's line ranges skip it, and this guard keeps it that way.

Requires ffmpeg and ffprobe on PATH, `pip install edge-tts pillow`, and a Consolas-like monospace font (FONT).
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
OUT = HERE / "out"
TMP = HERE / "tmp"
CAP = HERE / "captured"
FONT = os.environ.get("FONT", "C:/Windows/Fonts/consola.ttf")
FONT_BOLD = os.environ.get("FONT_BOLD", "C:/Windows/Fonts/consolab.ttf")

W, H, FPS = 1920, 1080, 24
SIZE, ROW = 34, 44                      # terminal font size and row pitch, in pixels
MARGIN, BAR = 26, 58                    # window margin and title bar height
PAD_X, PAD_Y = 30, 14                   # text inset inside the window
BODY_X, BODY_Y = MARGIN + PAD_X, MARGIN + BAR + PAD_Y
BODY_W, BODY_H = W - 2 * BODY_X, H - BODY_Y - MARGIN - PAD_Y

BG, WINDOW, BARC = "#0b0e14", "#11151c", "#1b212c"
TEXT, DIM, PROMPT = "#e6e6e6", "#9aa4b2", "#7ee787"
COLORS = {"red": (255, 92, 87), "green": (63, 200, 100), "amber": (242, 190, 70), "blue": (88, 166, 255)}

SCROLL_S, FADE_HL_S, TYPE_S = 0.6, 0.18, 0.55
FORBIDDEN = re.compile(r"ed25519:|near-credentials|secret_key|private_key", re.I)


def run_capture(name, spec):
    """Returns (shown command, text). Runs the command, or reads a file, and stores the text as it came."""
    if "run" in spec:
        env = dict(os.environ, PYTHONIOENCODING="utf-8")
        cmd = [sys.executable if c == "{python}" else c for c in spec["run"]]
        p = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, encoding="utf-8", timeout=120, env=env)
        if p.returncode != 0:
            sys.exit(f"capture {name}: {cmd} exited {p.returncode}\n{p.stderr}")
        text = p.stdout
    else:
        src = (ROOT / spec["file"]).read_text(encoding="utf-8").split("\n")
        text = "\n".join(line for a, b in spec["lines"] for line in src[a - 1:b]) + "\n"
    if FORBIDDEN.search(text):
        sys.exit(f"capture {name} holds key material, refusing to put it on screen: {FORBIDDEN.search(text).group(0)}")
    CAP.mkdir(exist_ok=True)
    (CAP / f"{name}.txt").write_text(text, encoding="utf-8", newline="\n")
    return spec["show"], text


class Term:
    """Terminal text laid out in rows, with one tall image of it. Lines wrap at spaces, as `fold -s` would."""

    def __init__(self, show, text, font, adv):
        self.font, self.adv = font, adv
        self.cols = int(BODY_W // adv)
        self.full = "$ " + show + "\n" + text.rstrip("\n")
        self.rows = []  # (text, offset of its first character in self.full)
        offset = 0
        for line in self.full.split("\n"):
            rest, at = line, offset
            while len(rest) > self.cols:
                cut = rest.rfind(" ", 1, self.cols + 1)
                cut = cut if cut > 0 else self.cols
                self.rows.append((rest[:cut], at))
                skip = cut + 1 if rest[cut:cut + 1] == " " else cut
                rest, at = rest[skip:], at + skip
            self.rows.append((rest, at))
            offset += len(line) + 1
        self.show = show
        self.canvas = Image.new("RGB", (BODY_W, len(self.rows) * ROW + BODY_H), WINDOW)
        d = ImageDraw.Draw(self.canvas)
        for i, (row, _) in enumerate(self.rows):
            if i == 0:
                d.text((0, i * ROW + 3), "$", font=font, fill=PROMPT)
                d.text((2 * adv, i * ROW + 3), row[2:], font=font, fill=TEXT)
            else:
                d.text((0, i * ROW + 3), row, font=font, fill=TEXT)

    def spans(self, sub):
        pos = self.full.find(sub)
        if pos < 0:
            sys.exit(f"not in the captured text, so it cannot be highlighted: {sub!r}")
        end, out = pos + len(sub), []
        for i, (row, at) in enumerate(self.rows):
            a, b = max(pos, at), min(end, at + len(row))
            if a < b:
                out.append((i, a - at, b - at))
        return out

    def row_of(self, sub):
        return self.spans(sub)[0][0]


def norm(s):
    return re.sub(r"[^a-z0-9]", "", s.lower())


async def tts(text, voice, rate, mp3):
    import edge_tts
    words = []
    com = edge_tts.Communicate(text, voice, rate=rate, boundary="WordBoundary")
    with open(mp3, "wb") as f:
        async for chunk in com.stream():
            if chunk["type"] == "audio":
                f.write(chunk["data"])
            elif chunk["type"] == "WordBoundary":
                words.append({"t": chunk["offset"] / 1e7, "d": chunk["duration"] / 1e7, "w": chunk["text"]})
    return words


def duration(path):
    out = subprocess.check_output(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)], timeout=30)
    return float(out.decode().strip())


def voice_scene(scene, sb):
    text = scene.get("tts", scene["narration"])
    key = hashlib.sha256(f"{sb['voice']}|{sb['rate']}|{text}".encode()).hexdigest()[:16]
    mp3, meta = TMP / f"{key}.mp3", TMP / f"{key}.json"
    if not (mp3.exists() and meta.exists()):
        meta.write_text(json.dumps(asyncio.run(tts(text, sb["voice"], sb["rate"], mp3))), encoding="utf-8")
    return mp3, json.loads(meta.read_text(encoding="utf-8")), duration(mp3)


def phrase_time(words, phrase):
    """Start time of a phrase in the spoken words, matched on letters and digits only."""
    joined, owner = "", []
    for i, w in enumerate(words):
        n = norm(w["w"])
        joined += n
        owner += [i] * len(n)
    at = joined.find(norm(phrase))
    if at < 0:
        sys.exit(f"phrase not spoken: {phrase!r}\nspoken: {' '.join(w['w'] for w in words)}")
    return words[owner[at]]["t"]


def plan(sb):
    """Works out scene start times and the absolute time of every beat."""
    t = 0.0
    for s in sb["scenes"]:
        s["mp3"], s["words"], s["voice_len"] = voice_scene(s, sb)
        lead, tail = s.get("lead", 0.3), s.get("tail", 0.45)
        s["start"] = t
        s["voice_at"] = t + lead
        s["voice_end"] = s["voice_at"] + s["words"][-1]["t"] + s["words"][-1]["d"]
        s["end"] = s["voice_end"] + tail  # the mp3 ends in silence, which may run under the next scene
        for b in s.get("beats", []):
            b["t"] = s["start"] if not b.get("on") else s["voice_at"] + phrase_time(s["words"], b["on"]) - 0.12
        t = s["end"]
    return t


class Scene:
    """What a terminal scene shows at a time. A scene may carry on from the state the last one ended in."""

    def __init__(self, s, terms, carried):
        self.s, self.terms = s, terms
        self.base = carried if s.get("continues") else {"cap": s.get("capture"), "scroll": 0.0, "upto": None}
        self.accumulate = s.get("accumulate", False)

    def state(self, t):
        cap, scroll, upto = self.base["cap"], self.base["scroll"], self.base["upto"]
        boxes = []
        for b in self.s.get("beats", []):
            if t < b["t"]:
                break
            if b.get("capture"):
                cap, scroll, upto, boxes = b["capture"], 0.0, None, []
            term = self.terms[cap]
            if "upto" in b:
                upto = term.spans(b["upto"])[-1][0] if b["upto"] else None
            if "top" in b:
                target = float(max(0, term.row_of(b["top"]) - b.get("top_row", 0)) * ROW)
                p = min(1.0, (t - b["t"]) / SCROLL_S)
                scroll += (target - scroll) * (p * p * (3 - 2 * p))
            if "hl" in b or b.get("clear"):
                boxes = [(x[0], x[1], x[2], True) for x in boxes] if self.accumulate and not b.get("clear") else []
                for h in b.get("hl", []):
                    boxes.append((h["text"], h["color"], b["t"], False))
        return cap, scroll, upto, boxes

    def final(self):
        cap, scroll, upto, _ = self.state(1e9)
        return {"cap": cap, "scroll": scroll, "upto": upto}


def chrome(title, small):
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)
    d.rounded_rectangle((MARGIN, MARGIN, W - MARGIN, H - MARGIN), 14, fill=WINDOW)
    d.rounded_rectangle((MARGIN, MARGIN, W - MARGIN, MARGIN + BAR), 14, fill=BARC)
    d.rectangle((MARGIN, MARGIN + BAR - 16, W - MARGIN, MARGIN + BAR), fill=BARC)
    for i, c in enumerate(("#ff5f56", "#ffbd2e", "#27c93f")):
        x = MARGIN + 26 + i * 28
        y = MARGIN + BAR // 2
        d.ellipse((x - 8, y - 8, x + 8, y + 8), fill=c)
    d.text((MARGIN + 126, MARGIN + BAR // 2), title, font=small, fill=DIM, anchor="lm")
    return img


def body_image(term, scroll, upto, boxes, t):
    y = int(round(scroll))
    body = term.canvas.crop((0, y, BODY_W, y + BODY_H))
    if upto is not None:
        hide = (upto + 1) * ROW - y
        if hide < BODY_H:
            ImageDraw.Draw(body).rectangle((0, max(0, hide), BODY_W, BODY_H), fill=WINDOW)
    if boxes:
        layer = Image.new("RGBA", (BODY_W, BODY_H), (0, 0, 0, 0))
        d = ImageDraw.Draw(layer)
        for text, color, t0, old in boxes:
            a = 0.6 if old else min(1.0, max(0.0, (t - t0) / FADE_HL_S))
            r, g, b = COLORS[color]
            for row, c0, c1 in term.spans(text):
                x0, x1 = c0 * term.adv - 7, c1 * term.adv + 7
                y0 = row * ROW - y
                d.rounded_rectangle((x0, y0, x1, y0 + ROW - 2), 7, fill=(r, g, b, int(52 * a)), outline=(r, g, b, int(255 * a)), width=3)
        body = Image.alpha_composite(body.convert("RGBA"), layer).convert("RGB")
    return body


def typing_image(term, t):
    body = Image.new("RGB", (BODY_W, BODY_H), WINDOW)
    d = ImageDraw.Draw(body)
    shown = term.show[: int(len(term.show) * min(1.0, t / TYPE_S))]
    d.text((0, 3), "$", font=term.font, fill=PROMPT)
    d.text((2 * term.adv, 3), shown, font=term.font, fill=TEXT)
    d.rectangle(((2 + len(shown)) * term.adv + 2, 6, (3 + len(shown)) * term.adv, ROW - 6), fill=TEXT)
    return body


def title_card(lines, font):
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)
    gap = 104
    y = H // 2 - gap * (len(lines) - 1) // 2
    for i, line in enumerate(lines):
        d.text((W // 2, y + i * gap), line, font=font, fill=TEXT, anchor="mm")
    return img


def end_card(card, repo_url, fonts):
    big, mid, small = fonts
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)
    x, y = 150, 165
    d.text((x, y), card["name"], font=big, fill=TEXT)
    y += 130
    for line in card["lines"]:
        d.text((x, y), line, font=small, fill=DIM)
        y += 50
    y += 44
    for line in card["facts"]:
        d.text((x, y), line, font=small, fill=TEXT)
        y += 50
    y += 56
    link = repo_url or card["repo_fallback"]
    d.text((x, y), link, font=mid, fill=tuple(COLORS["blue"]))
    y += 100
    d.text((x, y), card["status"], font=big, fill=tuple(COLORS["amber"]))
    return img


def render(sb, total, terms, cards, small, tmp_video):
    cmd = ["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}", "-r", str(FPS), "-i", "-",
           "-vf", "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p", "-c:v", "libx264", "-preset", "medium", "-crf", "17",
           "-g", str(FPS * 2), "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", str(tmp_video)]
    ff = subprocess.Popen(cmd, stdin=subprocess.PIPE)
    carried, windows = None, {}
    scenes = []
    for s in sb["scenes"]:
        sc = Scene(s, terms, carried) if s["kind"] == "term" else None
        if sc:
            carried = sc.final()
        scenes.append((s, sc))
    frames = int(round(total * FPS))
    i_scene, last_key, last_bytes = 0, None, None
    for n in range(frames):
        t = n / FPS
        while i_scene + 1 < len(scenes) and t >= scenes[i_scene + 1][0]["start"]:
            i_scene += 1
        s, sc = scenes[i_scene]
        local = t - s["start"]
        if sc is None:
            fade = min(1.0, local / 0.35, (s["end"] - t) / 0.35 if s.get("fade_out") else 1.0)
            key = (s["id"], round(fade, 2))
            if key != last_key:
                img = cards[s["id"]] if fade >= 1 else Image.blend(Image.new("RGB", (W, H), BG), cards[s["id"]], max(0.0, fade))
                last_bytes = img.tobytes()
        else:
            cap, scroll, upto, boxes = sc.state(t)
            term = terms[cap]
            typing = s.get("type") and local < TYPE_S + 0.12
            alphas = tuple((b[0], b[3], round(min(1.0, (t - b[2]) / FADE_HL_S), 2)) for b in boxes)
            key = (s["id"], cap, typing and local, round(scroll), upto, alphas)
            if key != last_key:
                title = s.get("window", sb["window"])
                if title not in windows:
                    windows[title] = chrome(title, small)
                img = windows[title].copy()
                img.paste(typing_image(term, local) if typing else body_image(term, scroll, upto, boxes, t), (BODY_X, BODY_Y))
                last_bytes = img.tobytes()
        last_key = key
        ff.stdin.write(last_bytes)
    ff.stdin.close()
    if ff.wait(timeout=300) != 0:
        sys.exit("ffmpeg failed while rendering")


def mix_audio(sb, total, wav):
    cmd = ["ffmpeg", "-y", "-loglevel", "error"]
    parts = []
    for i, s in enumerate(sb["scenes"]):
        cmd += ["-i", str(s["mp3"])]
        ms = int(round(s["voice_at"] * 1000))
        parts.append(f"[{i}:a]aresample=48000,adelay={ms}:all=1[a{i}]")
    n = len(sb["scenes"])
    labels = "".join(f"[a{i}]" for i in range(n))
    graph = ";".join(parts) + f";{labels}amix=inputs={n}:normalize=0:duration=longest,apad,atrim=0:{total:.3f},alimiter=limit=0.89[out]"
    cmd += ["-filter_complex", graph, "-map", "[out]", "-ac", "2", "-ar", "48000", str(wav)]
    subprocess.run(cmd, check=True, timeout=120)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--plan", action="store_true", help="print the timing and stop")
    ap.add_argument("--repo-url", help="repository link for the end card")
    args = ap.parse_args()
    sb = json.loads((HERE / "storyboard.json").read_text(encoding="utf-8"))
    OUT.mkdir(exist_ok=True)
    TMP.mkdir(exist_ok=True)

    font = ImageFont.truetype(FONT, SIZE)
    adv = font.getlength("M" * 100) / 100
    terms = {name: Term(*run_capture(name, spec), font, adv) for name, spec in sb["captures"].items()}
    total = plan(sb) + sb.get("hold", 0.6)
    sb["scenes"][-1]["end"] = total

    rows = []
    for s in sb["scenes"]:
        rows.append({"scene": s["id"], "script": s.get("script_time"), "start": round(s["start"], 2), "voice": [round(s["voice_at"], 2), round(s["voice_end"], 2)],
                     "beats": [{"t": round(b["t"], 2), "on": b.get("on", "")} for b in s.get("beats", [])]})
        print(f"{s['id']:10} script {s.get('script_time', ''):5} start {s['start']:6.2f} voice {s['voice_at']:6.2f}-{s['voice_end']:6.2f}")
        for b in s.get("beats", []):
            print(f"    {b['t']:6.2f}  {b.get('on', '')}")
    print(f"total {total:.2f}s, cols {terms['replay'].cols}, rows in view {BODY_H // ROW}")
    (OUT / "timing.json").write_text(json.dumps({"total": total, "scenes": rows}, indent=1), encoding="utf-8")
    if args.plan:
        return

    # The end card's text has to be the repository's own words, and the story's own numbers.
    readme = " ".join((ROOT / "README.md").read_text(encoding="utf-8").split())
    for line in sb["end_card"]["readme_sentences"]:
        if line not in readme:
            sys.exit(f"end card text is not in README.md: {line!r}")
    for line in sb["end_card"]["facts"]:
        if line not in terms["story"].full:
            sys.exit(f"end card fact is not in the story.py output: {line!r}")

    # Every highlight and scroll anchor must exist before a single frame is drawn.
    cap = None
    for s in sb["scenes"]:
        if s["kind"] != "term":
            continue
        cap = s.get("capture", cap)
        for b in s.get("beats", []):
            cap = b.get("capture", cap)
            for sub in [h["text"] for h in b.get("hl", [])] + [b[k] for k in ("top", "upto") if b.get(k)]:
                terms[cap].spans(sub)

    small = ImageFont.truetype(FONT, 26)
    card = sb["end_card"]
    cards = {
        "title": title_card(sb["title_card"], ImageFont.truetype(FONT, 72)),
        "end": end_card(card, args.repo_url or sb.get("repo_url"), (ImageFont.truetype(FONT_BOLD, 76), ImageFont.truetype(FONT, 46), ImageFont.truetype(FONT, 36))),
    }
    video, wav = TMP / "video.mp4", TMP / "voice.wav"
    render(sb, total, terms, cards, small, video)
    mix_audio(sb, total, wav)

    final = OUT / sb["output"]
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", str(video), "-i", str(wav), "-c:v", "copy", "-c:a", "aac", "-b:a", "160k",
                    "-movflags", "+faststart", "-shortest", str(final)], check=True, timeout=120)
    upload = OUT / sb["output_small"]
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", str(final), "-c:v", "libx264", "-preset", "slow", "-crf", "23", "-pix_fmt", "yuv420p",
                    "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart", str(upload)],
                   check=True, timeout=300)
    for p in (final, upload):
        print(f"{p}  {p.stat().st_size / 1e6:.1f} MB  {duration(p):.2f}s")


if __name__ == "__main__":
    main()
