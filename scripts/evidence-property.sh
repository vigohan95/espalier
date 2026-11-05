#!/usr/bin/env bash
# Regenerates the in-process evidence: 100,000 operations, then the mutation check.
S=/mnt/d/income-engine/work/hackathons/near/scripts
bash $S/property.sh
bash $S/mutants.sh 4 1500 | tail -22
