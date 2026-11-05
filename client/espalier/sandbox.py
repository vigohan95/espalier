"""Starts a real local NEAR node and hands back a funded root account. Used by the demos and the
trials. `NEAR_SANDBOX_BIN_PATH` points at the near-sandbox binary, the same one near-workspaces uses."""
import json
import os
import shutil
import subprocess
import tempfile
import time
import urllib.request

from . import near


class Sandbox:
    def __init__(self, port=3041):
        self.port = port
        self.bin = os.environ.get("NEAR_SANDBOX_BIN_PATH") or shutil.which("near-sandbox")
        if not self.bin:
            raise SystemExit("set NEAR_SANDBOX_BIN_PATH to a near-sandbox binary (see README)")
        self.home = tempfile.mkdtemp(prefix="espalier-sandbox-")
        self.proc = None

    def __enter__(self):
        subprocess.run([self.bin, "--home", self.home, "init"], check=True, capture_output=True, timeout=60)
        # Default garbage collection keeps 5 epochs of 500 blocks, about 40 minutes of past heights,
        # so age and code history read here the same way they read on an archival mainnet node.
        path = os.path.join(self.home, "config.json")
        config = json.load(open(path))
        config["rpc"]["addr"] = f"127.0.0.1:{self.port}"
        config["network"]["addr"] = f"127.0.0.1:{self.port + 1}"
        json.dump(config, open(path, "w"))
        self.log = open(os.path.join(self.home, "node.log"), "w")
        self.proc = subprocess.Popen([self.bin, "--home", self.home, "run"], stdout=self.log, stderr=self.log)
        url = f"http://127.0.0.1:{self.port}"
        for _ in range(120):
            try:
                with urllib.request.urlopen(url + "/status", timeout=2) as r:
                    json.load(r)
                break
            except OSError:
                time.sleep(0.5)
        else:
            raise RuntimeError("sandbox did not start")
        self.rpc = near.Rpc(url)
        key_file = json.load(open(os.path.join(self.home, "validator_key.json")))
        self.root = near.Signer(self.rpc, key_file["account_id"], near.Key.from_string(key_file["secret_key"]))
        while self.rpc.block()["header"]["height"] < 3:
            time.sleep(0.3)
        return self

    def __exit__(self, *exc):
        if self.proc:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        self.log.close()
        shutil.rmtree(self.home, ignore_errors=True)
