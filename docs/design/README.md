# Design docs

Design notes and spikes for obsidian-ee, newest first. Filenames follow `YYYY-MM-DD-issue-NN-slug.md`; the milestone doc omits `issue-NN` because it spans six issues.

| Doc | Status | Summary |
|---|---|---|
| [Milestone: Local end-to-end verification backbone](2026-07-24-e2e-verification-milestone.md) | approved — milestone #1 + issues #46–52 created | Build the Tier-2 multi-process MLS-over-relay wire test that gives the hollow E2E fixtures teeth, using the local docker-compose relay (no cloud deploy). Doubles as #28's acceptance harness. |
| [Spike: MLS in WASM (#28)](2026-07-23-issue-28-mls-in-wasm-spike.md) | spike complete — Route 1 confirmed end-to-end | openmls→wasm vs ts-mls vs mls-rs. Route 1 (compile the existing openmls stack to wasm) wins on interop-by-construction; both stages GREEN (compile + headless-browser run, epoch=1, zero clock errors). |
| [Unblock clean build (#24) + real compiled-WASM tests (#26)](2026-07-23-issue-24-26-bdd-realwasm.md) | landed (`bc30a9f`) | BDD-first: build WASM on demand and exercise the real compiled binary in Jest via a load-real-wasm helper, rather than committing the artifact. |
