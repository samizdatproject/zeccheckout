#!/usr/bin/env python3
"""Builds a captioned 1920x1080 slideshow MP4 from slides.json (image or terminal text + caption).

Each slide is rendered as HTML by headless Chrome (ffmpeg here has no drawtext),
then concatenated with ffmpeg. Also writes an .srt and muxes it as a soft track.
Usage: mkvideo.py slides.json outdir chrome_profile
"""
import html, json, os, subprocess, sys, time

slides = json.load(open(sys.argv[1]))
out = sys.argv[2]
profile = sys.argv[3]
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
work = os.path.join(out, "_frames")
os.makedirs(work, exist_ok=True)

CSS = """
html,body{margin:0;width:1920px;height:1080px;background:#141210;color:#f4efe6;font-family:-apple-system,Helvetica,sans-serif;overflow:hidden}
.stage{position:absolute;top:40px;left:0;right:0;height:820px;display:flex;align-items:center;justify-content:center}
.stage img{max-width:1700px;max-height:800px;border-radius:14px;box-shadow:0 10px 40px rgba(0,0,0,.6)}
.term{width:1640px;height:760px;background:#0b0b0b;border:1px solid #333;border-radius:14px;padding:28px 34px;box-sizing:border-box;
 font:26px/1.45 Menlo,monospace;color:#d8d8d8;white-space:pre-wrap;overflow:hidden}
.term b{color:#f5c542;font-weight:normal}
.title{font-size:84px;font-weight:700;text-align:center;line-height:1.15}
.title small{display:block;font-size:40px;font-weight:400;color:#cbbfa8;margin-top:28px}
.cap{position:absolute;left:80px;right:80px;bottom:46px;min-height:120px;display:flex;align-items:center;justify-content:center;
 text-align:center;font-size:40px;line-height:1.3;background:rgba(0,0,0,.72);border-radius:12px;padding:18px 30px;box-sizing:border-box}
.tag{position:absolute;top:18px;right:34px;font-size:24px;color:#a99f8c}
"""


def render(i, s):
    if "image" in s:
        body = f'<div class="stage"><img src="file://{os.path.abspath(s["image"])}"></div>'
    elif "term" in s:
        body = f'<div class="stage"><div class="term">{s["term"]}</div></div>'
    else:
        body = f'<div class="stage"><div class="title">{s["title"]}</div></div>'
    tag = f'<div class="tag">{html.escape(s.get("tag", ""))}</div>'
    cap = f'<div class="cap">{html.escape(s["caption"])}</div>' if s.get("caption") else ""
    page = f"<!doctype html><meta charset=utf-8><style>{CSS}</style>{tag}{body}{cap}"
    hp = os.path.join(work, f"s{i:02d}.html")
    open(hp, "w").write(page)
    png = os.path.join(work, f"s{i:02d}.png")
    if os.path.exists(png):
        os.remove(png)
    p = subprocess.Popen([CHROME, "--headless=new", "--disable-gpu", f"--user-data-dir={profile}", "--window-size=1920,1080",
                          "--hide-scrollbars", "--allow-file-access-from-files", f"--screenshot={png}", "file://" + hp],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(60):
        if os.path.exists(png) and os.path.getsize(png) > 0:
            break
        time.sleep(0.5)
    time.sleep(0.5)
    p.kill()
    subprocess.run(["pkill", "-f", profile])
    return png


def ts(t):
    ms = int(round(t * 1000))
    return f"{ms // 3600000:02d}:{ms // 60000 % 60:02d}:{ms // 1000 % 60:02d},{ms % 1000:03d}"


lst, srt, t = [], [], 0.0
for i, s in enumerate(slides):
    png = render(i, s)
    lst.append(f"file '{png}'\nduration {s['secs']}")
    if s.get("caption"):
        srt.append(f"{len(srt) + 1}\n{ts(t)} --> {ts(t + s['secs'])}\n{s['caption']}\n")
    t += s["secs"]
lst.append(f"file '{png}'")  # concat demuxer needs the last file repeated
open(os.path.join(work, "list.txt"), "w").write("\n".join(lst) + "\n")
srt_path = os.path.join(out, "zeccheckout-demo.srt")
open(srt_path, "w").write("\n".join(srt))
mp4 = os.path.join(out, "zeccheckout-demo.mp4")
subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i", os.path.join(work, "list.txt"),
                "-i", srt_path, "-map", "0:v", "-map", "1:s", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-r", "30",
                "-vf", "scale=1920:1080", "-c:s", "mov_text", "-metadata:s:s:0", "language=eng", "-t", f"{t:.2f}", mp4], check=True)
print(f"{mp4} {t:.0f}s, {len(slides)} slides")
