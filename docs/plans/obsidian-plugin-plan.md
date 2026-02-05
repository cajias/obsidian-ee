# Obsidian Plugin Implementation Plan

## Overview

Build an Obsidian plugin for E2E encrypted collaborative editing using WASM bridge to `collab-core`.

## Architecture Decisions

- **WASM bridge**: Compile `collab-core` to WASM via `collab-wasm` crate
- **MVP scope**: Single document sync between two users
- **Testing**: Hybrid (mocked unit tests + Playwright E2E)
- **Infrastructure**: Mock relay for fast tests, Docker Compose for E2E
- **Location**: Monorepo at `plugins/obsidian-ee/`

## Phase 1: WASM Foundation

### Task 1.1: Create collab-wasm crate
- [ ] Create `crates/collab-wasm/Cargo.toml` with wasm-bindgen
- [ ] Add wasm32-unknown-unknown target
- [ ] Basic skeleton that exports a greeting function

### Task 1.2: WASM build pipeline
- [ ] Add `wasm-pack` build script
- [ ] Test: WASM module loads in Node.js
- [ ] Output to `plugins/obsidian-ee/src/wasm/`

### Task 1.3: Yrs document exposure
- [ ] Test: Can create Yrs document via WASM
- [ ] Test: Can apply update and read text
- [ ] Expose `CollabCore` struct with `new()`, `apply_update()`, `get_text()`

## Phase 2: Plugin Skeleton

### Task 2.1: Scaffold Obsidian plugin
- [ ] Create `plugins/obsidian-ee/` directory structure
- [ ] `package.json` with dependencies (obsidian, esbuild, jest)
- [ ] `manifest.json` for Obsidian
- [ ] `tsconfig.json` with WASM support
- [ ] `esbuild.config.mjs` build configuration

### Task 2.2: Plugin entry point
- [ ] Test: Plugin class instantiates without error (mocked obsidian)
- [ ] `main.ts` with `CollabPlugin extends Plugin`
- [ ] WASM module loading in `onload()`

### Task 2.3: CollabClient class
- [ ] Test: CollabClient connects to WebSocket
- [ ] Test: CollabClient handles reconnection
- [ ] `collab-client.ts` with WebSocket management

## Phase 3: Editor Integration

### Task 3.1: Editor sync binding
- [ ] Test: Local edit creates CRDT update
- [ ] `editor-sync.ts` with CodeMirror binding
- [ ] Capture editor changes, convert to Yrs transactions

### Task 3.2: Remote update application
- [ ] Test: Remote update applies to editor
- [ ] Apply incoming CRDT updates to CodeMirror
- [ ] Handle cursor preservation

## Phase 4: Encryption Layer

### Task 4.1: MLS in WASM
- [ ] Test: MLS group initialization works
- [ ] Expose MLS functions via wasm-bindgen
- [ ] `init_mls()`, `process_welcome()`, `create_commit()`

### Task 4.2: Encrypt/decrypt integration
- [ ] Test: Encrypt/decrypt round-trip succeeds
- [ ] Integrate encryption into `create_update()`
- [ ] Integrate decryption into `apply_update()`

## Phase 5: E2E Integration

### Task 5.1: E2E test infrastructure
- [ ] `e2e/mock-relay.ts` lightweight WebSocket server
- [ ] `e2e/fixtures/test-vault/` minimal Obsidian vault
- [ ] Docker Compose service for Playwright

### Task 5.2: The E2E test
- [ ] `e2e/two-user-sync.spec.ts` (write first, expect failure)
- [ ] Wire all components together
- [ ] Debug until test passes

## Success Criteria

```
npm test          # All unit/integration tests pass
npm run e2e       # two-user-sync.spec.ts passes
```

## File Structure (Final)

```
obsidian-ee/
├── crates/
│   ├── collab-wasm/
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   └── ...
├── plugins/obsidian-ee/
│   ├── src/
│   │   ├── main.ts
│   │   ├── collab-client.ts
│   │   ├── editor-sync.ts
│   │   ├── wasm/
│   │   │   ├── collab_wasm.js
│   │   │   └── collab_wasm_bg.wasm
│   │   └── __tests__/
│   │       ├── collab-client.test.ts
│   │       └── editor-sync.test.ts
│   ├── e2e/
│   │   ├── mock-relay.ts
│   │   ├── fixtures/test-vault/
│   │   └── two-user-sync.spec.ts
│   ├── package.json
│   ├── tsconfig.json
│   ├── esbuild.config.mjs
│   └── manifest.json
└── ...
```
