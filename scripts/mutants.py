#!/usr/bin/env python3
"""Does the property test actually catch bugs? Each mutant below breaks the contract in one place,
then the in-process property test runs against the broken copy. A mutant counts as caught only if
the test fails. A surviving mutant means the test suite has a hole. Takes about 10 minutes."""
import json, os, shutil, subprocess, sys, tempfile, time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRATCH = os.environ.get("ESPALIER_BUILD", os.path.join(tempfile.gettempdir(), "espalier-build"))
WORK = os.path.join(SCRATCH, "mutant-src")
TARGET = os.path.join(SCRATCH, "mutant-target")

LIB, WIN = "contract/src/lib.rs", "contract/src/window.rs"

MUTANTS = [
    ("cap_checks_leaf_only", "subtree bound: cap is checked on the leaf only", LIB,
     """                .numbers(g.cap, used, amount));
            }
            let in_window""",
     """                .numbers(g.cap, used, amount));
            }
            if id != gid { cur = g.parent; continue; }
            let in_window"""),
    ("cap_ignores_ancestors", "subtree bound: headroom walk stops at the leaf", LIB,
     """                .numbers(g.window_cap, in_window, amount));
            }
            cur = g.parent;""",
     """                .numbers(g.window_cap, in_window, amount));
            }
            cur = None;"""),
    ("cap_ignores_reserved", "in-flight spends are invisible to the cap check", LIB,
     "            let used = g.spent + g.reserved;\n            if amount > g.cap - used {",
     "            let used = g.spent;\n            if amount > g.cap - used {"),
    ("revoke_ignores_ancestors", "cascade: lineage walk looks at the grant itself only", LIB,
     """                return Err(Denial::new(code, format!("grant {id} expired at {}", g.expires_at_ms)).at(id));
            }
            cur = g.parent;""",
     """                return Err(Denial::new(code, format!("grant {id} expired at {}", g.expires_at_ms)).at(id));
            }
            cur = None;"""),
    ("rollback_keeps_window", "failed spend leaves the window charged", LIB,
     "                    g.window.sub(p.at_ms, g.window_ms, p.amount);",
     "                    let _ = (p.at_ms, g.window_ms);"),
    ("settle_leaf_only", "settlement updates only the leaf", LIB,
     """                    g.window.sub(p.at_ms, g.window_ms, p.amount);
                }
                root = id;
                cur = g.parent;""",
     """                    g.window.sub(p.at_ms, g.window_ms, p.amount);
                }
                root = id;
                cur = None;"""),
    ("rollback_counts_as_spent", "failed spend still counts as spent", LIB,
     "                if ok {\n                    g.spent += p.amount;\n                } else {",
     "                if ok || true {\n                    g.spent += p.amount;\n                } else {"),
    ("window_undercounts", "window forgets spends early", WIN,
     "(now_ms / width(window_ms)).saturating_sub(BUCKETS)",
     "(now_ms / width(window_ms)).saturating_sub(BUCKETS / 2)"),
    ("att_per_tx_off", "attenuation: per_tx not checked", LIB,
     "if t.per_tx.0 > p.per_tx {", "if false && t.per_tx.0 > p.per_tx {"),
    ("att_cap_nominal", "attenuation: child cap against the nominal cap, not the remainder", LIB,
     "if t.cap.0 > remaining {", "if t.cap.0 > p.cap {"),
    ("att_methods_off", "attenuation: methods not checked", LIB,
     "if bits & !p.methods != 0 {", "if false && bits & !p.methods != 0 {"),
    ("att_recipients_off", "attenuation: recipient subset not checked", LIB,
     "if let Some(extra) = asked.iter().find(|a| !list.contains(a)) {",
     "if let Some(extra) = asked.iter().find(|a| false && !list.contains(a)) {"),
    ("att_depth_off", "attenuation: depth not checked", LIB,
     "if p.max_depth == 0 || t.max_depth > p.max_depth - 1 {", "if false {"),
    ("allowlist_off", "recipient allowlist not enforced at spend time", LIB,
     "            if !list.contains(recipient) {\n", "            if false && !list.contains(recipient) {\n"),
    ("hash_not_checked", "execute_approved accepts changed arguments", LIB,
     "if self.spend_hash(a.grant, asset, recipient, amount, memo, id) != a.hash {",
     "if false && self.spend_hash(a.grant, asset, recipient, amount, memo, id) != a.hash {"),
    ("approval_not_consumed", "approval can be replayed", LIB,
     "        self.approvals.remove(&approval_id);\n        self.grants.get_mut(&approval.grant)",
     "        self.grants.get_mut(&approval.grant)"),
    ("denial_charges_budget", "a denial changes financial state", LIB,
     "        g.denied = g.denied.saturating_add(1);",
     "        g.denied = g.denied.saturating_add(1);\n        g.gas_left = g.gas_left.saturating_sub(1);"),
    ("no_auto_revoke", "N denials do not revoke", LIB,
     "} else if denied >= max && !already {", "} else if false && denied >= max && !already {"),
    ("listed_is_ignored", "the registry says listed and the spend goes on", LIB,
     "        if answer.listed {\n", "        if false && answer.listed {\n"),
    ("registry_fails_open", "no answer from the registry counts as clear", LIB,
     "let bytes = env::promise_result_checked(0, 1024).map_err(|_| unavailable())?;",
     "let Ok(bytes) = env::promise_result_checked(0, 1024) else { return Ok(()) };"),
    ("garbage_counts_as_clear", "an unreadable answer counts as clear", LIB,
     "let answer: ScreenAnswer = near_sdk::serde_json::from_slice(&bytes).map_err(|_| unavailable())?;",
     "let Ok(answer) = near_sdk::serde_json::from_slice::<ScreenAnswer>(&bytes) else { return Ok(()) };"),
    ("screen_skips_liveness", "a revoke during screening does not stop the spend", LIB,
     "            .and_then(|_| self.check_live(gid, now_ms()))\n            .and_then(|_| self.check_not_paused());",
     "            .and_then(|_| self.check_not_paused());"),
    ("held_not_recorded", "funds waiting for the registry are not held", LIB,
     "            self.native_held += amount;\n", "            let _ = amount;\n"),
    ("ticket_signature_off", "any signature passes", LIB,
     "if !env::ed25519_verify(&signature, &message, &key) {",
     "if false && !env::ed25519_verify(&signature, &message, &key) {"),
    ("no_ticket_goes_through", "a bound grant spends with no ticket and no human", LIB,
     "let no_clearance = self.grant(gid).clearance && clearance.is_none();",
     "let no_clearance = false;"),
    ("att_clearance_off", "attenuation: a child can drop clearance", LIB,
     "if p.clearance && !t.clearance_required {", "if false && p.clearance && !t.clearance_required {"),
    ("contact_not_recorded", "settlement does not write the contact", LIB,
     "                self.note_contact(gid, &p.recipient, p.amount);\n", ""),
    ("contact_on_failure", "a failed payment writes a contact", LIB,
     "            if ok {\n                self.note_contact(gid", "            if ok || true {\n                self.note_contact(gid"),
]


def run_test(seeds, ops):
    env = dict(os.environ, CARGO_TARGET_DIR=TARGET, PROPERTY_SEEDS=str(seeds), PROPERTY_OPS=str(ops))
    t0 = time.time()
    p = subprocess.run(
        ["cargo", "test", "--release", "random_operations", "--", "--nocapture"],
        cwd=f"{WORK}/contract", env=env, capture_output=True, text=True, timeout=900,
    )
    out = p.stdout + p.stderr
    reason = ""
    for line in out.splitlines():
        if "panicked at" in line or line.startswith("I") or "assertion" in line:
            reason = line.strip()[:160]
            if "panicked at" not in line:
                break
    return p.returncode, reason, time.time() - t0, out


def main():
    seeds, ops = int(sys.argv[1]) if len(sys.argv) > 1 else 4, int(sys.argv[2]) if len(sys.argv) > 2 else 1500
    results = []
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(WORK)
    shutil.copytree(f"{REPO}/contract", f"{WORK}/contract")
    # copytree keeps mtimes, and cargo would then reuse the last mutant's binary as the baseline.
    for name in (LIB, WIN):
        os.utime(f"{WORK}/{name}")
    with open(f"{WORK}/contract/Cargo.toml", "a") as f:
        f.write('\n[workspace]\n\n[profile.release]\nopt-level = 2\noverflow-checks = true\n')
    rc, _, secs, out = run_test(seeds, ops)
    if rc != 0:
        print(out[-3000:])
        sys.exit("baseline failed: the unmodified contract must pass first")
    print(f"baseline passes in {secs:.0f}s")
    for name, what, path, old, new in MUTANTS:
        src = open(f"{REPO}/{path}", encoding="utf8").read()
        if src.count(old) != 1:
            sys.exit(f"mutant {name}: pattern found {src.count(old)} times")
        open(f"{WORK}/{path}", "w", encoding="utf8").write(src.replace(old, new))
        rc, reason, secs, out = run_test(seeds, ops)
        if "error: could not compile" in out:
            sys.exit(f"mutant {name} does not compile, so it proves nothing")
        caught = rc != 0
        print(f"{'caught  ' if caught else 'SURVIVED'} {name}: {what} ({secs:.0f}s) {reason}", flush=True)
        results.append({"mutant": name, "what": what, "caught": caught, "first_failure": reason})
        open(f"{WORK}/{path}", "w", encoding="utf8").write(src)
    doc = {
        "what": "each mutant breaks one line of the contract; the in-process property test must fail",
        "seeds": seeds,
        "ops_per_seed": ops,
        "caught": sum(r["caught"] for r in results),
        "total": len(results),
        "mutants": results,
    }
    json.dump(doc, open(f"{REPO}/docs/evidence/mutants.json", "w"), indent=2)
    print(f"{doc['caught']}/{doc['total']} caught")


main()
