# Single entry point for every check in this repo, split by WHAT A TEST NEEDS:
# `test` needs nothing, `test-e2e` needs Docker (and AWS for the deployment
# verification), `lint` needs the linters. CI invokes these same targets, so a
# developer running `make test` runs the CI job's implementation, not a copy of
# it. This is a dispatch table, not a build system — keep it dumb.

.PHONY: help test test-e2e lint

help:
	@echo "obsidian-ee — make targets"
	@echo
	@echo "  make test       No Docker, no AWS: Rust workspace + CDK type-check and assertions"
	@echo "  make test-e2e   Needs Docker; tests/deployment-verify.sh additionally needs AWS"
	@echo "  make lint       fmt + clippy, cargo-deny, actionlint, shellcheck"
	@echo
	@echo "  cargo-deny, actionlint and shellcheck are skipped with a notice when absent."

# A real file target, deliberately NOT .PHONY: `npm ci` reruns only when the
# lockfile is newer than the installed tree. npm ci recreates the directory, but
# touch keeps the timestamp ordering true even if it does not.
infra/node_modules: infra/package-lock.json
	cd infra && npm ci
	@touch infra/node_modules

test: infra/node_modules
	cargo test --workspace
# `cdk.json` and `npm test` both run ts-node in --transpile-only mode, so the
# type-check below is the ONLY type check in the whole pipeline: without it a
# type error ships green. `--no-install` is load-bearing too — it must fail
# loudly rather than silently fetching some other tsc.
	cd infra && npx --no-install tsc --noEmit
	cd infra && npm test

test-e2e:
	cargo xtask e2e
# Exit 2 from deployment-verify.sh is BLOCKED — the outstanding steps are human
# (AWS bootstrap, dispatch approvals) and named in the script's own output,
# which passes through unredirected. Only exit 1 is a failure.
	bash tests/deployment-verify.sh; rc=$$?; if [ $$rc -eq 2 ]; then echo "BLOCKED (exit 2): outstanding human steps, see above — not a failure"; exit 0; fi; exit $$rc

lint:
	cargo xtask lint
# `if/else` rather than `cmd && ... || echo skip`: the latter prints "skip" when
# the tool IS installed and FAILS, turning a real finding into a green run.
	@if command -v cargo-deny >/dev/null 2>&1; then cargo deny check; else echo "skip: cargo-deny not installed (cargo install cargo-deny --locked --version 0.20.2)"; fi
	@if command -v actionlint >/dev/null 2>&1; then actionlint .github/workflows/release.yml .github/workflows/deploy.yml .github/workflows/integration.yml; else echo "skip: actionlint not installed (https://github.com/rhysd/actionlint/releases)"; fi
	@if command -v shellcheck >/dev/null 2>&1; then shellcheck tests/deployment-verify.sh; else echo "skip: shellcheck not installed (brew install shellcheck)"; fi
