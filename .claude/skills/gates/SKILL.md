---
name: gates
description: Run the AWS-free gate set and report what is green. Use when asked to "run the gates", "check what passes locally", or before opening a PR on this repo.
disable-model-invocation: true
---

Run `cargo xtask gates` from the workspace root. Report each gate's PASS / FAIL /
SKIP line and the summary, then stop.

The gate list lives in `GATES` in `xtask/src/main.rs`, kept byte-identical to the
matching `integration.yml` steps. Do not restate or re-implement it here — a second
copy is a copy that drifts. Add a gate by editing that list.

Gates needing AWS credentials (`run-m3.sh`, `run-m4.sh`) are deliberately excluded.
