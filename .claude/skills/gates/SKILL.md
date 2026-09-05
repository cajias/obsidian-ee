---
name: gates
description: Run the AWS-free gate set and report what is green. Use when asked to "run the gates", "check what passes locally", or before opening a PR on this repo.
disable-model-invocation: true
---

Run `make test` and then `make lint` from the workspace root. Report each
target's PASS / FAIL line — including any `skip:` notice `make lint` prints for a
linter that is not installed locally — then stop.

Those two targets are the AWS-free set: `make test` needs neither Docker nor AWS
(Rust workspace tests, which include the design-integrity and deploy-workflow
guards, plus the CDK type check and assertions), and `make lint` needs only the
linters. The Makefile is the single definition — CI runs the same targets — so do
not restate or re-implement the command list here. Add a check by editing the
Makefile.

`make test-e2e` is deliberately excluded: it needs a running Docker daemon, and
`tests/deployment-verify.sh` inside it needs AWS credentials.
