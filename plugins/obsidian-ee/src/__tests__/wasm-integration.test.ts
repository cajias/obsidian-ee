/**
 * WASM Integration Tests for CollabCore.
 *
 * These exercise the REAL compiled WASM artifact built on demand at `src/wasm/`
 * (run scripts/build-wasm.sh; gitignored, not committed) — collab_wasm.js +
 * collab_wasm_bg.wasm, loaded exactly as main.ts loads it in Node:
 * readFileSync -> WebAssembly.compile -> init(module). No mocks.
 *
 * The assertions encode the verified boundary contract from
 * crates/collab-wasm/src/lib.rs — including the fact that thrown errors are
 * PLAIN OBJECTS `{ type, message }` (via `From<CollabError> for JsValue`),
 * not Error instances, so jest's `.toThrow()` (which reads Error.message)
 * cannot match them. We catch and `toMatchObject` instead.
 */
import { describe, it, expect, beforeAll } from '@jest/globals';
import { loadRealWasm, newCore } from './helpers/load-real-wasm';

/** Capture a thrown value (plain object OR Error) without unwrapping it. */
function catchThrown(fn: () => void): unknown {
    try {
        fn();
        return undefined;
    } catch (error) {
        return error;
    }
}

const KEY_32 = (fill: number): Uint8Array => new Uint8Array(32).fill(fill);

beforeAll(async () => {
    await loadRealWasm();
});

describe('CollabCore WASM — module load / lifecycle', () => {
    it('loads the compiled artifact and exposes a working CollabCore constructor', async () => {
        const { CollabCore } = await loadRealWasm();
        const core = new CollabCore();
        expect(core.get_text()).toBe('');
        core.free();
    });

    it('has_encryption_key is false on a fresh core (proves __wbg_ptr wired)', async () => {
        const core = await newCore();
        expect(core.has_encryption_key()).toBe(false);
        core.free();
    });

    it('throws when using a freed core', async () => {
        const core = await newCore();
        core.free();
        expect(() => core.get_text()).toThrow(/null pointer passed to rust/);
    });
});

describe('CollabCore WASM — basic CRDT ops', () => {
    it('given empty doc, when inserting at positions, then text reflects both inserts', async () => {
        const core = await newCore();
        core.insert(0, 'Hello, World!');
        core.insert(5, ',X');
        expect(core.get_text()).toBe('Hello,X, World!');
        core.free();
    });

    it('given empty doc, when inserting out of range, then it clamps and does not throw', async () => {
        const core = await newCore();
        expect(() => core.insert(100, 'x')).not.toThrow();
        expect(core.get_text()).toBe('x');
        core.free();
    });

    it('given "Hello, World!", when deleting a range, then remaining text is "World!"', async () => {
        const core = await newCore();
        core.insert(0, 'Hello, World!');
        core.delete(0, 7);
        expect(core.get_text()).toBe('World!');
        core.free();
    });

    it('given "abc", when deleting out of range, then it clamps like insert (no throw)', async () => {
        const core = await newCore();
        core.insert(0, 'abc');
        // length overruns content -> clamps to end, no throw, instance usable
        expect(() => core.delete(2, 50)).not.toThrow();
        expect(core.get_text()).toBe('ab');
        // index past end -> no-op, instance still usable
        expect(() => core.delete(10, 5)).not.toThrow();
        expect(core.get_text()).toBe('ab');
        core.free();
    });

    it('given unicode text, when inserted, then it round-trips through UTF-8/UTF-16 exactly', async () => {
        const core = await newCore();
        const s = 'a😀b́';
        core.insert(0, s);
        expect(core.get_text()).toBe(s);
        core.free();
    });
});

describe('CollabCore WASM — state sync', () => {
    it('given a snapshot, when the doc is later edited, then the snapshot is unaffected', async () => {
        const core = await newCore();
        core.insert(0, 'first');
        const snapshot = core.encode_state();
        const before = Uint8Array.from(snapshot);
        core.insert(0, 'more ');
        expect(snapshot).toEqual(before);
        expect(snapshot.length).toBeGreaterThan(0);
        expect(snapshot).toBeInstanceOf(Uint8Array);
        // Pin that the snapshot carries real doc state (not just non-empty bytes):
        // decoding it into a fresh core round-trips the text captured at snapshot time.
        const restored = await newCore();
        restored.apply_update(snapshot);
        expect(restored.get_text()).toBe('first');
        restored.free();
        core.free();
    });

    it('given docA state, when docB applies it, then docB matches docA', async () => {
        const docA = await newCore();
        const docB = await newCore();
        docA.insert(0, 'Hello from A');
        docB.apply_update(docA.encode_state());
        expect(docB.get_text()).toBe('Hello from A');
        docA.free();
        docB.free();
    });

    it('given garbage bytes, when applied as an update, then it throws sync_error', async () => {
        const core = await newCore();
        const err = catchThrown(() => core.apply_update(new Uint8Array([255, 255, 255, 255])));
        expect(err).toMatchObject({ type: 'sync_error' });
        core.free();
    });

    it('given an edited doc, when encoding the state vector, then it is a non-empty owned Uint8Array', async () => {
        const core = await newCore();
        core.insert(0, 'content here');
        const sv = core.encode_state_vector();
        expect(sv).toBeInstanceOf(Uint8Array);
        expect(sv.length).toBeGreaterThan(0);
        // Prove the vector reflects clock state, not a constant stub: an empty
        // doc's state vector must differ from the edited doc's.
        const emptyCore = await newCore();
        const svEmpty = emptyCore.encode_state_vector();
        expect(Buffer.from(sv).equals(Buffer.from(svEmpty))).toBe(false);
        emptyCore.free();
        core.free();
    });

    it('given two synced docs, when each makes a concurrent edit and they cross-apply, then both converge', async () => {
        const docA = await newCore();
        const docB = await newCore();

        // Start equal: seed A, sync B from A.
        docA.insert(0, 'base');
        docB.apply_update(docA.encode_state());
        expect(docB.get_text()).toBe('base');

        // Concurrent edits: A prepends 'X', B appends 'Y'.
        docA.insert(0, 'X');
        docB.insert(docB.get_text().length, 'Y');

        // Cross-apply each other's full state.
        const stateA = docA.encode_state();
        const stateB = docB.encode_state();
        docA.apply_update(stateB);
        docB.apply_update(stateA);

        expect(docA.get_text()).toBe(docB.get_text());
        // Non-overlapping deterministic edits: X prepended at 0 to 'base', Y
        // appended at end. Assert the EXACT merge so a bug that drops the
        // untouched 'base' segment (yielding 'XY') fails instead of passing.
        expect(docA.get_text()).toBe('XbaseY');

        docA.free();
        docB.free();
    });
});

describe('CollabCore WASM — encryption round-trip + errors', () => {
    it('given a valid 32-byte key, when set, then has_encryption_key is true', async () => {
        const core = await newCore();
        core.set_encryption_key(KEY_32(7));
        expect(core.has_encryption_key()).toBe(true);
        core.free();
    });

    it('given a wrong-length key, when set, then it throws key_error and key stays unset', async () => {
        const core = await newCore();
        const err = catchThrown(() => core.set_encryption_key(new Uint8Array(16)));
        expect(err).toMatchObject({ type: 'key_error', message: 'Key must be 32 bytes' });
        expect(core.has_encryption_key()).toBe(false);
        core.free();
    });

    it('given a keyed core, when encrypting then decrypting, then plaintext round-trips with nonce+tag framing', async () => {
        const core = await newCore();
        core.set_encryption_key(KEY_32(7));
        const pt = new TextEncoder().encode('secret payload');
        const ct = core.encrypt(pt);
        expect(ct.length).toBe(12 + pt.length + 16);
        expect([...ct]).not.toEqual([...pt]);
        const decrypted = core.decrypt(ct);
        expect([...decrypted]).toEqual([...pt]);
        core.free();
    });

    it('given no key, when encrypting, then it throws key_error "No encryption key set"', async () => {
        const core = await newCore();
        const err = catchThrown(() => core.encrypt(new Uint8Array([1, 2, 3])));
        expect(err).toMatchObject({ type: 'key_error', message: 'No encryption key set' });
        core.free();
    });

    it('given a keyed core, when decrypting ciphertext shorter than 12 bytes, then it throws decryption "Ciphertext too short"', async () => {
        const core = await newCore();
        core.set_encryption_key(KEY_32(7));
        const errEmpty = catchThrown(() => core.decrypt(new Uint8Array(0)));
        const errShort = catchThrown(() => core.decrypt(new Uint8Array(8)));
        expect(errEmpty).toMatchObject({ type: 'decryption', message: 'Ciphertext too short' });
        expect(errShort).toMatchObject({ type: 'decryption', message: 'Ciphertext too short' });
        core.free();
    });

    it('given valid ciphertext, when a byte after the nonce is flipped, then decrypt throws decryption', async () => {
        const core = await newCore();
        core.set_encryption_key(KEY_32(7));
        const ct = core.encrypt(new TextEncoder().encode('tamper me'));
        ct[12] ^= 0xff; // flip a ciphertext byte (past the 12-byte nonce)
        const err = catchThrown(() => core.decrypt(ct));
        expect(err).toMatchObject({ type: 'decryption' });
        core.free();
    });

    it('given two cores with the same key, when B applies A encrypted state, then B matches A', async () => {
        const docA = await newCore();
        const docB = await newCore();
        docA.set_encryption_key(KEY_32(7));
        docB.set_encryption_key(KEY_32(7));
        docA.insert(0, 'Encrypted sync!');
        docB.apply_update_encrypted(docA.encode_state_encrypted());
        expect(docB.get_text()).toBe('Encrypted sync!');
        docA.free();
        docB.free();
    });

    it('given two cores with different keys, when B applies A encrypted state, then it throws decryption and B stays empty', async () => {
        const docA = await newCore();
        const docB = await newCore();
        docA.set_encryption_key(KEY_32(1));
        docB.set_encryption_key(KEY_32(2));
        docA.insert(0, 'Secret message');
        const err = catchThrown(() => docB.apply_update_encrypted(docA.encode_state_encrypted()));
        expect(err).toMatchObject({ type: 'decryption' });
        expect(docB.get_text()).toBe('');
        docA.free();
        docB.free();
    });
});
