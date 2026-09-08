'use strict';

// Jest requires the testEnvironment module directly in the worker process's
// own (host) realm -- unlike setupFiles/setupFilesAfterEnv, which run inside
// the sandboxed vm.Context Jest creates per test file. A failing BigInt
// assertion (several suites assert MLS epochs this way, e.g.
// `expect(epochOf(client)).toBe(2n)`) attaches the raw BigInt to the error's
// matcher result, which the worker reports back to the main process over
// `child_process.send()` -- serialized with plain `JSON.stringify` in the
// HOST realm. `JSON.stringify` cannot serialize BigInt, so the worker crashes
// mid-report:
//
//   TypeError: Do not know how to serialize a BigInt
//     at messageParent (jest-worker/build/workers/messageParent.js)
//
// which surfaces as "Test suite failed to run" / "Tests: 0 total" -- visually
// identical to a broken import, hiding the real assertion diff entirely.
// Patching `BigInt.prototype.toJSON` from *inside* the sandbox (the commonly
// suggested `setupFilesAfterEnv` fix) does NOT help: `ToObject` on a
// primitive uses the realm of whichever code calls it, and `JSON.stringify`
// here runs in the host realm, not the sandbox. This file is required by
// jest-runner itself (host realm) before it ever creates the sandbox, so the
// patch lands where the crash actually happens.
BigInt.prototype.toJSON = function () {
  return this.toString();
};

module.exports = require('jest-environment-node');
