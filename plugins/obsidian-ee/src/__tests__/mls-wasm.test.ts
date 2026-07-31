/**
 * MLS-in-WASM surface tests (issue #28) against the REAL compiled wasm.
 *
 * These exercise `collab-core`'s MLS engine across the wasm-bindgen boundary:
 * a positive in-process round-trip plus four negative-path (trust-boundary)
 * assertions. MLS-surface errors are real JS `Error`s (asserted with
 * `toThrow`), UNLIKE the AES `CollabCore` path which returns `{type, message}`.
 */
import { loadRealWasm } from './helpers/load-real-wasm';

type Wasm = Awaited<ReturnType<typeof loadRealWasm>>;

let wasm: Wasm;

beforeAll(async () => {
    wasm = await loadRealWasm();
});

/** True if `needle`'s bytes appear as a contiguous window in `haystack`. */
function containsBytes(haystack: Uint8Array, needle: Uint8Array): boolean {
    if (needle.length === 0 || needle.length > haystack.length) {
        return false;
    }
    for (let i = 0; i <= haystack.length - needle.length; i++) {
        let match = true;
        for (let j = 0; j < needle.length; j++) {
            if (haystack[i + j] !== needle[j]) {
                match = false;
                break;
            }
        }
        if (match) {
            return true;
        }
    }
    return false;
}

describe('MLS WASM surface', () => {
    it('round-trips an encrypted update between two group members', () => {
        // Arrange: Alice owns doc1, Bob joins via a Welcome.
        const { WasmEncryptedDocument, generate_key_package } = wasm;
        const bobPending = generate_key_package('bob');
        const alice = WasmEncryptedDocument.create('doc1', 'alice');
        const invite = alice.create_invite(bobPending.key_package);
        const bob = WasmEncryptedDocument.join(invite, bobPending);

        // Act: Alice edits and ships the encrypted op to Bob.
        alice.insert(0, 'Hello');
        const op = alice.get_encrypted_update();
        bob.apply_encrypted_update(op.ciphertext, op.epoch);

        // Assert: Bob decrypts to the same content; both at epoch 1.
        // `epoch` is a Rust u64, marshalled to JS as a BigInt.
        expect(bob.get_content()).toBe('Hello');
        expect(alice.epoch).toBe(1n);
        expect(bob.epoch).toBe(1n);

        // The plaintext must not appear verbatim in the ciphertext.
        const plaintext = new TextEncoder().encode('Hello');
        expect(containsBytes(op.ciphertext, plaintext)).toBe(false);
    });

    it('rejects a ciphertext from a different group (cross-group replay)', () => {
        // Arrange: group1 = alice + bob; carol is in an unrelated doc2 group.
        const { WasmEncryptedDocument, generate_key_package } = wasm;
        const bobPending = generate_key_package('bob');
        const alice = WasmEncryptedDocument.create('doc1', 'alice');
        const invite = alice.create_invite(bobPending.key_package);
        WasmEncryptedDocument.join(invite, bobPending);
        const carol = WasmEncryptedDocument.create('doc2', 'carol');

        // Act: alice produces a real op for her group.
        alice.insert(0, 'secret');
        const op = alice.get_encrypted_update();

        // Assert: carol (no shared group) cannot apply it; stays empty.
        expect(() => carol.apply_encrypted_update(op.ciphertext, op.epoch)).toThrow();
        expect(carol.get_content()).toBe('');
    });

    it('rejects a join with a Welcome sealed to a different member', () => {
        // Arrange: invite is created for Carol's init key, not Bob's.
        const { WasmEncryptedDocument, generate_key_package } = wasm;
        const bobPending = generate_key_package('bob');
        const carolPending = generate_key_package('carol');
        const alice = WasmEncryptedDocument.create('doc1', 'alice');
        const inviteForCarol = alice.create_invite(carolPending.key_package);

        // Assert: Bob cannot open a Welcome that isn't his.
        expect(() => WasmEncryptedDocument.join(inviteForCarol, bobPending)).toThrow();
    });

    it('consumes the pending member on join (handle nulled)', () => {
        // Arrange + Act: a successful join moves the Rust value out of bobPending.
        const { WasmEncryptedDocument, generate_key_package } = wasm;
        const bobPending = generate_key_package('bob');
        const alice = WasmEncryptedDocument.create('doc1', 'alice');
        const invite = alice.create_invite(bobPending.key_package);
        WasmEncryptedDocument.join(invite, bobPending);

        // Assert: touching the moved handle traps (wasm-bindgen null-pointer guard).
        expect(() => bobPending.key_package).toThrow(/null pointer passed to rust/);
    });

    it('clamps an out-of-range delete instead of trapping the wasm instance', () => {
        // Arrange: a real two-member group so this exercises the full MLS surface.
        const { WasmEncryptedDocument, generate_key_package } = wasm;
        const bobPending = generate_key_package('bob');
        const alice = WasmEncryptedDocument.create('doc1', 'alice');
        const invite = alice.create_invite(bobPending.key_package);
        WasmEncryptedDocument.join(invite, bobPending);
        alice.insert(0, 'Hello');

        // Act: delete overruns content (index 2, len 50 on a 5-char doc).
        // Pre-fix this panicked -> `RuntimeError: unreachable`, poisoning the instance.
        // Assert: no throw, content clamped to the deletion of index 2..end.
        expect(() => alice.delete(2, 50)).not.toThrow();
        expect(alice.get_content()).toBe('He');
    });

    it('exposes no key-injection path on the MLS document', () => {
        // MLS keys are derived by the group; there is no all-zeros/placeholder entry.
        const { WasmEncryptedDocument } = wasm;
        const doc = WasmEncryptedDocument.create('doc1', 'alice');
        expect(
            (doc as unknown as { set_encryption_key?: unknown }).set_encryption_key
        ).toBeUndefined();
        expect(
            (doc as unknown as { has_encryption_key?: unknown }).has_encryption_key
        ).toBeUndefined();
    });

    it('exposes no AES surface anywhere on the loaded WASM MODULE (AES-256-GCM path removed)', async () => {
        // Module-level assertion (not just one document): the whole AES CollabCore
        // surface is gone from the compiled artifact after #28. RED before the AES
        // removal (CollabCore + set/has_encryption_key were exported), GREEN after.
        const mod = (await import('../wasm/collab_wasm')) as unknown as Record<string, unknown>;
        expect(mod.CollabCore).toBeUndefined();
        expect(mod.set_encryption_key).toBeUndefined();
        expect(mod.has_encryption_key).toBeUndefined();
        // The MLS surface is what remains.
        expect(mod.WasmEncryptedDocument).toBeDefined();
        expect(mod.generate_key_package).toBeDefined();
    });
});
