#!/usr/bin/env python3
"""Says what will work on this machine before you try it, and checks the repository is in order.
Standard library only, no network unless you pass --online.

    python3 scripts/preflight.py [--online]
"""
import json
import os
import re
import shutil
import subprocess
import sys
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FAILED = []


def row(status, text):
    print(f"[{status:4}] {text}")
    if status == "FAIL":
        FAILED.append(text)


def tool(name, why):
    path = shutil.which(name)
    row("ok" if path else "skip", f"{name}: {path or 'not found, needed only for ' + why}")


def main():
    row("ok" if sys.version_info >= (3, 9) else "FAIL", f"python {sys.version.split()[0]}, need 3.9 or newer")
    for p in ("LICENSE", "README.md", "llms.txt", "docs/ATTACKS.md", "docs/BASELINE-GAPS.md", "docs/RECIPIENT-CHECK.md", "docs/AGENTS.md",
              "res/espalier_vault.wasm", "res/espalier_registry.wasm", "docs/evidence/sandbox-report.json", "docs/evidence/testnet-run.json",
              "docs/evidence/trials/report.md", "docs/evidence/mainnet-signals.md", "docs/submission/PAYLOAD.md"):
        row("ok" if os.path.exists(os.path.join(ROOT, p)) else "FAIL", p)

    # Secrets: only public keys may appear. A secret key in base58 runs to 80 characters or more.
    hits = []
    pattern = re.compile(r'(secret_key|private_key)"?\s*[:=]\s*"?ed25519:|ed25519:[1-9A-HJ-NP-Za-km-z]{80,}')
    for d, dirs, files in os.walk(ROOT):
        dirs[:] = [x for x in dirs if x not in (".git", "target", ".logs", "__pycache__")]
        for f in files:
            if f.endswith((".wasm", ".lock")):
                continue
            p = os.path.join(d, f)
            try:
                if pattern.search(open(p, encoding="utf8", errors="ignore").read()):
                    hits.append(os.path.relpath(p, ROOT))
            except OSError:
                pass
    row("ok" if not hits else "FAIL", f"no secret keys in the tree {hits or ''}")

    git = subprocess.run(["git", "-C", ROOT, "log", "-1", "--format=%h %cI"], capture_output=True, text=True)
    row("ok" if git.returncode == 0 else "skip", f"git head {git.stdout.strip()}")
    first = subprocess.run(["git", "-C", ROOT, "log", "--reverse", "--format=%cI"], capture_output=True, text=True).stdout.split("\n")[0]
    row("ok" if first >= "2026-10-19" else "FAIL", f"first commit {first}, inside the hackathon window, so there is no baseline to separate (rules Q-003)")

    print("\nFor the optional steps:")
    tool("cargo", "the sandbox scenarios and property tests")
    tool("cargo-near", "rebuilding the wasm")
    sandbox = os.environ.get("NEAR_SANDBOX_BIN_PATH")
    row("ok" if sandbox and os.path.exists(sandbox) else "skip", f"NEAR_SANDBOX_BIN_PATH: {sandbox or 'unset; near-workspaces fetches it, the Python demos need it set'}")

    if "--online" in sys.argv:
        deploy = json.load(open(os.path.join(ROOT, "docs", "evidence", "testnet-deploy.json")))
        body = json.dumps({"jsonrpc": "2.0", "id": "x", "method": "query", "params": {"request_type": "view_account", "finality": "final", "account_id": deploy["vault"]}}).encode()
        req = urllib.request.Request("https://test.rpc.fastnear.com", body, {"Content-Type": "application/json", "User-Agent": "espalier-preflight"})
        try:
            with urllib.request.urlopen(req, timeout=20) as r:
                res = json.load(r)["result"]
            row("ok", f"testnet vault {deploy['vault']} answers, code hash {res['code_hash'][:12]}")
        except Exception as e:
            row("FAIL", f"testnet RPC: {e}")

    print("\nNext: python3 scripts/story.py, then python3 scripts/verify-testnet.py")
    sys.exit(1 if FAILED else 0)


main()
