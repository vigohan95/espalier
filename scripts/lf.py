#!/usr/bin/env python3
"""Makes every source file in the repo use LF. Windows editors and Python's text mode add CR."""
import os, sys
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXT = (".rs", ".py", ".md", ".sh", ".toml", ".json", ".txt", ".log", ".lock", ".jsonl", ".gitignore", ".gitattributes")
n = 0
for d, dirs, files in os.walk(ROOT):
    dirs[:] = [x for x in dirs if x not in (".git", "target", ".logs", "__pycache__")]
    for f in files:
        if f.endswith(EXT) or f == "LICENSE":
            p = os.path.join(d, f)
            b = open(p, "rb").read()
            if b"\r\n" in b:
                open(p, "wb").write(b.replace(b"\r\n", b"\n"))
                n += 1
print(f"{n} files normalised")
