import {
    WasmEncryptedDocument,
    WasmInvite,
    WasmPendingMember,
    generate_key_package,
} from './wasm/collab_wasm';
import type { WasmEncryptedOp } from './wasm/collab_wasm';

export type CollabRole = 'owner' | 'joiner';

// Inbound frames come from an UNTRUSTED relay (zero-knowledge threat model), so
// network-fed collections are bounded by BYTES before any allocation: a huge
// frame would otherwise flow JSON.parse -> number[] -> Uint8Array -> Rust
// Vec<u8> and OOM the Electron renderer. Legit frames are already capped at
// 1 MiB by the relay itself (MAX_MESSAGE_SIZE in collab-relay/src/relay.rs);
// 2 MiB here leaves slack for relay-added fields. Measured on the raw JSON
// text (UTF-16 code units ~= bytes for this ASCII protocol), which strictly
// bounds every array parsed out of it.
const MAX_INBOUND_FRAME_BYTES = 2 * 1024 * 1024;

// Lifetime of a minted subscribe capability (#72), mirroring the CLI's
// CAPABILITY_TTL_SECS (crates/collab-cli/src/commands.rs): short enough that a
// leaked capability expires quickly, long enough to outlive a handshake.
const CAPABILITY_TTL_SECS = 300n;

/** Whole seconds since the Unix epoch, as the u64 the wasm boundary expects. */
function nowUnix(): bigint {
    return BigInt(Math.max(0, Math.floor(Date.now() / 1000)));
}

// Reconnect policy defaults, mirroring the Rust side so the two implementations
// of this logic cannot drift again: MIN_STABLE_CONNECTION in
// crates/collab-cli/src/commands.rs and RetryPolicy::default()'s max_delay in
// crates/collab-core/src/connection.rs.
const DEFAULT_MIN_STABLE_CONNECTION_MS = 10000;
const DEFAULT_MAX_RECONNECT_DELAY_MS = 30000;
const DEFAULT_MAX_RECONNECT_ATTEMPTS = 5;

/**
 * Narrow view of WasmVaultSync (#32): the client only needs to apply remote
 * manifest updates; local file-event handling stays with the plugin.
 */
export interface VaultSyncLike {
    apply_remote_manifest(update: Uint8Array): string[];
}

export interface CollabClientConfig {
    relayUrl: string;
    userId: string;
    docId: string;
    role: CollabRole; // owner creates the MLS group; joiner joins via a Welcome
    /**
     * Enables vault-manifest sync when provided (#32). The manifest rides the
     * same relay connection as its OWN MLS group on `manifestDocId`, established
     * by the same owner/joiner handshake as the file doc — never a shared key.
     */
    vaultSync?: VaultSyncLike;
    /** The locally-trusted manifest doc id (from wasm `manifest_doc_id()`). */
    manifestDocId?: string;
    /**
     * How long a connection must stay up before the reconnect budget is
     * refilled. Mirrors the Rust CLI's `MIN_STABLE_CONNECTION`
     * (crates/collab-cli/src/commands.rs). Defaults to
     * `DEFAULT_MIN_STABLE_CONNECTION_MS`.
     */
    minStableConnectionMs?: number;
    /**
     * Ceiling on the exponential reconnect backoff. Mirrors
     * `RetryPolicy::max_delay` (crates/collab-core/src/connection.rs). Defaults
     * to `DEFAULT_MAX_RECONNECT_DELAY_MS`.
     */
    maxReconnectDelayMs?: number;
    /**
     * How many reconnect attempts to spend before giving up with
     * `max_retries_exceeded`. Mirrors `RetryPolicy::max_retries`
     * (crates/collab-core/src/connection.rs). Defaults to
     * `DEFAULT_MAX_RECONNECT_ATTEMPTS`; 0 gives up on the first drop.
     */
    maxReconnectAttempts?: number;
}

export type UpdateCallback = (text: string) => void;
export type ManifestPathsCallback = (newPaths: string[]) => void | Promise<void>;
export type DisconnectCallback = (reason: string) => void;
export type ErrorCallback = (error: CollabError) => void;
export type ConnectionState = 'connected' | 'connecting' | 'disconnected' | 'reconnecting';

export interface CollabError {
    type: 'decryption' | 'connection' | 'sync';
    message: string;
    docId?: string;
    originalError?: Error;
}

export interface YrsUpdateMessage {
    type: 'yrs_update';
    encrypted: number[];
    doc_id?: string;
    epoch?: number;
}

export interface MlsHandshakeMessage {
    type: 'mls_handshake';
    doc_id?: string;
    payload: number[];
    message_type: 'key_package' | 'welcome' | 'commit';
}

/**
 * Interface for WASM CollabError objects returned from Rust.
 * These are plain JS objects with type and message fields.
 */
interface WasmCollabError {
    type: string;
    message: string;
}

/**
 * Type guard to check if an error is a WASM CollabError object.
 */
function isWasmCollabError(error: unknown): error is WasmCollabError {
    return (
        typeof error === 'object' &&
        error !== null &&
        'type' in error &&
        'message' in error &&
        typeof (error as WasmCollabError).type === 'string' &&
        typeof (error as WasmCollabError).message === 'string'
    );
}

/**
 * Extract error message from various error types including WASM errors.
 * WASM errors are plain objects that would produce "[object Object]" with String().
 * Exported so callers outside this module (e.g. main.ts) don't reimplement it.
 */
export function extractErrorMessage(error: unknown): string {
    if (error instanceof Error) {
        return error.message;
    }
    if (isWasmCollabError(error)) {
        return `[${error.type}] ${error.message}`;
    }
    return String(error);
}

/**
 * Validation error thrown when CollabClientConfig has invalid values.
 */
export class ConfigValidationError extends Error {
    constructor(message: string) {
        super(message);
        this.name = 'ConfigValidationError';
    }
}

/**
 * Validate CollabClientConfig values at runtime.
 * Throws ConfigValidationError if validation fails.
 */
function validateConfig(config: CollabClientConfig): void {
    // Validate relayUrl
    if (!config.relayUrl || typeof config.relayUrl !== 'string') {
        throw new ConfigValidationError('relayUrl must be a non-empty string');
    }
    if (!config.relayUrl.startsWith('ws://') && !config.relayUrl.startsWith('wss://')) {
        throw new ConfigValidationError('relayUrl must start with ws:// or wss://');
    }

    // Validate userId
    if (!config.userId || typeof config.userId !== 'string') {
        throw new ConfigValidationError('userId must be a non-empty string');
    }

    // Validate docId
    if (!config.docId || typeof config.docId !== 'string') {
        throw new ConfigValidationError('docId must be a non-empty string');
    }

    // Validate role
    if (config.role !== 'owner' && config.role !== 'joiner') {
        throw new ConfigValidationError('role must be "owner" or "joiner"');
    }

    validateVaultSyncConfig(config);
}

/**
 * Validate the vault-sync pairing (#32): the manifest doc id names a SEPARATE
 * MLS group riding the same connection, so it must be present and must differ
 * from the file doc id — a collision would route file and manifest frames to
 * the same group and defeat per-group isolation.
 */
function validateVaultSyncConfig(config: CollabClientConfig): void {
    if (!config.vaultSync) {
        return;
    }
    if (!config.manifestDocId || typeof config.manifestDocId !== 'string') {
        throw new ConfigValidationError(
            'manifestDocId must be a non-empty string when vaultSync is provided'
        );
    }
    if (config.manifestDocId === config.docId) {
        throw new ConfigValidationError('manifestDocId must differ from docId');
    }
}

/**
 * The anchor material a `register_doc_key` frame carries (#29). Structurally
 * the wasm `WasmAnchorRotation`, so a rotation emitted by a commit is passed
 * straight through; a first (TOFU) registration is built by hand with an empty
 * `rotation_proof`.
 */
interface DocKeyAnchor {
    epoch: bigint;
    public_key: Uint8Array;
    proof: Uint8Array;
    rotation_proof: Uint8Array;
}

/**
 * A view onto one MLS group's mutable doc/pending slot (#32). The file group
 * (`doc`/`pending`) and the manifest group (`manifestDoc`/`manifestPending`)
 * run the exact same owner/joiner bootstrap and key_package/welcome/commit
 * handshake — a `GroupSlot` lets that logic be written once and run twice,
 * against the two real field pairs, instead of duplicated per group.
 */
interface GroupSlot {
    docId: string;
    getDoc: () => WasmEncryptedDocument | null;
    setDoc: (doc: WasmEncryptedDocument | null) => void;
    getPending: () => WasmPendingMember | null;
    setPending: (pending: WasmPendingMember | null) => void;
}

export class CollabClient {
    private ws: WebSocket | null = null;
    private doc: WasmEncryptedDocument | null = null;
    private pending: WasmPendingMember | null = null;
    // Second, independent MLS group for the vault manifest (#32). Same lifecycle
    // as `doc`/`pending`, keyed on config.manifestDocId. null until established
    // (owner) or until the Welcome arrives (joiner).
    private manifestDoc: WasmEncryptedDocument | null = null;
    private manifestPending: WasmPendingMember | null = null;
    // Terminal: set by destroy(), which frees the groups. Post-destroy() slot
    // state is indistinguishable from a never-connected client's, so nothing
    // derivable stops a later connect() from bootstrapping a fresh epoch-0
    // group — this flag does.
    private destroyed = false;
    private config: CollabClientConfig;
    private onUpdateCallback: UpdateCallback | null = null;
    private onManifestPathsCallback: ManifestPathsCallback | null = null;
    private onDisconnectCallback: DisconnectCallback | null = null;
    private onErrorCallback: ErrorCallback | null = null;
    private reconnectAttempts = 0;
    // Lifecycle state, NOT the retry budget: true once disconnect() has stopped
    // this client, cleared by connect(). The budget lives in config
    // (maxReconnectAttempts) and is never mutated — overloading it as the stop
    // flag made a configured budget of 0 indistinguishable from "user stopped".
    private stopped = false;
    private reconnectDelay = 1000;
    private messageQueue: object[] = [];
    private readonly maxQueueSize = 1000;
    private connectionState: ConnectionState = 'disconnected';
    private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
    private stabilityTimer: ReturnType<typeof setTimeout> | null = null;
    private isInitialConnect = true;
    private connectPromise: Promise<void> | null = null;

    constructor(config: CollabClientConfig) {
        validateConfig(config);
        this.config = config;
    }

    /** The file-group slot, viewing `this.doc`/`this.pending`. */
    private fileSlot(): GroupSlot {
        return {
            docId: this.config.docId,
            getDoc: () => this.doc,
            setDoc: (doc) => {
                this.doc = doc;
            },
            getPending: () => this.pending,
            setPending: (pending) => {
                this.pending = pending;
            },
        };
    }

    /** The manifest-group slot (#32), viewing `this.manifestDoc`/`this.manifestPending`. */
    private manifestSlot(): GroupSlot {
        return {
            docId: this.config.manifestDocId!,
            getDoc: () => this.manifestDoc,
            setDoc: (doc) => {
                this.manifestDoc = doc;
            },
            getPending: () => this.manifestPending,
            setPending: (pending) => {
                this.manifestPending = pending;
            },
        };
    }

    /**
     * Bootstrap one MLS group `slot` as owner (create immediately) or joiner
     * (ship a single-use key package as an mls_handshake frame; the slot's doc
     * stays null until the Welcome arrives). Shared by the file group and the
     * manifest group (#32) — same roles, same wire shape, only the doc id and
     * the slot being written differ.
     */
    private bootstrapGroup(slot: GroupSlot): void {
        if (this.config.role === 'owner') {
            slot.setDoc(WasmEncryptedDocument.create(slot.docId, this.config.userId));
            // The group exists now, so the relay's verification anchor can be
            // registered — nothing a member mints verifies without it (#72).
            // Presenting the capability is establishGroup's job, deliberately
            // after this: see the comment there.
            this.registerAnchor(slot);
            return;
        }
        const pending = generate_key_package(this.config.userId);
        slot.setPending(pending);
        this.send({
            type: 'mls_handshake',
            doc_id: slot.docId,
            payload: [...pending.key_package],
            message_type: 'key_package',
        });
    }

    /**
     * Establish the MLS group(s) EXACTLY ONCE EACH, per slot. MLS group state is
     * long-lived and persists across reconnects (only `this.ws` is recreated),
     * so onopen must NOT re-run this for a slot that already has one:
     * re-creating would spawn a NEW empty solo group at epoch 0 and orphan the
     * real group. The per-slot filter below satisfies the CLAUDE.md
     * reconnect-lifecycle invariant "guard against a second start that would
     * orphan the prior client/handle."
     *
     * Per-slot, not one flag over both: a vault-sync joiner can be admitted to
     * the file group and NOT the manifest group (the Welcomes are independent),
     * and an all-or-nothing latch left that second group permanently unjoinable.
     *
     * ponytail: first-cut behavior — a joiner whose socket drops mid-handshake
     * (before the Welcome) stays un-joined and fails closed (no plaintext), which
     * is strictly better than a divergent group. A mid-handshake resume state
     * machine is deliberately NOT built here (YAGNI).
     */
    private establishGroup(): void {
        // Vault sync (#32): the manifest is a SEPARATE MLS group on manifestDocId,
        // established by the same handshake. Same role: whoever owns the file doc
        // owns the manifest group. Throws below fail the connect() attempt via
        // tryInitialize, exactly like the file group.
        const slots =
            this.config.vaultSync && this.config.manifestDocId
                ? [this.fileSlot(), this.manifestSlot()]
                : [this.fileSlot()];
        // A slot with a live doc is a group to RESUME; a slot with a live pending
        // is a joiner still mid-handshake, whose one-time key package is already
        // on the wire. Only a slot with neither needs bootstrapping.
        const toBootstrap = slots.filter((slot) => !slot.getDoc() && !slot.getPending());
        try {
            toBootstrap.forEach((slot) => this.bootstrapGroup(slot));
        } catch (error) {
            // bootstrapGroup throws AFTER slot.setDoc(), so a half-done attempt
            // would otherwise leave the doc set with register_doc_key never sent.
            // Every later connect would then skip that slot's bootstrap and
            // present capabilities for a document the relay has no anchor for —
            // it rejects the subscribe, and the client goes silently deaf while
            // resolving normally. Undo the whole attempt instead, so a retry
            // re-runs it from scratch.
            //
            // Safe to discard the docs because every step that can throw in there
            // runs BEFORE its frame is sent: the retry's fresh TOFU registration
            // is still the relay's first for the document.
            //
            // Exactly the slots THIS call tried to bootstrap: a group that
            // survived a disconnect is not part of this attempt, and freeing it
            // here would leave the client deaf on a document it is still a
            // member of. Same reason this teardown is not in failConnect, which
            // is shared with the identify/subscribe branch that also fires on a
            // RECONNECT whose long-lived group must survive.
            toBootstrap.forEach((slot) => this.freeSlot(slot));
            throw error;
        }
        // Anchors are registered, so the capability-less subscribes sent moments
        // ago in subscribe() can be upgraded to content-authorized (#72). This
        // runs OUTSIDE the teardown above on purpose: minting is the one step
        // that happens after register_doc_key is already on the wire, and
        // discarding the doc then would strand a relay-side anchor a fresh TOFU
        // registration cannot replace (the relay demands a rotation continuity
        // proof once an anchor exists). A throw here still fails the connect; the
        // group survives and the next attempt's subscribe() re-presents.
        //
        // Only the slots THIS call bootstrapped: a slot that already had a group
        // was re-presented moments ago by subscribe(), so re-minting it here is a
        // wasted wasm call.
        toBootstrap.filter((slot) => slot.getDoc()).forEach((slot) => this.subscribeTo(slot));
    }

    /** True when this frame's doc_id is the configured manifest group. */
    private isManifestFrame(docId: string | undefined): boolean {
        return (
            this.config.vaultSync !== undefined &&
            this.config.manifestDocId !== undefined &&
            docId === this.config.manifestDocId
        );
    }

    /**
     * Shared by every onopen failure branch (identify/subscribe send failure,
     * establishGroup throwing): close and drop the socket, then reject this
     * connect() attempt. Without settling here, connectPromise would never
     * resolve/reject and the dedup guard in connect() would return the same
     * stale, never-settling promise forever.
     */
    private failConnect(reject: (reason?: unknown) => void, error: unknown): void {
        console.error('[CollabClient]', error);
        this.ws?.close();
        this.ws = null;
        reject(error);
    }

    /**
     * Run the whole onopen initialization, rejecting this connect() attempt and
     * tearing down the socket if any of it throws instead of letting the throw
     * abort onopen unhandled. Every step is a wasm-bindgen `Result<T, JsError>`
     * call away from throwing: establishGroup() creates groups/key packages, and
     * subscribe() now MINTS a capability (#72). Without this guard
     * connectPromise would never settle (the exact hang class CLAUDE.md's
     * connect-settles-exactly-once invariant names).
     */
    private tryInitialize(reject: (reason?: unknown) => void): boolean {
        try {
            const identified = this.sendIdentify();
            const subscribed = this.subscribe();
            if (!identified || !subscribed) {
                throw new Error('Failed to send initialization messages');
            }
            this.establishGroup();
            return true;
        } catch (error) {
            this.failConnect(reject, error);
            return false;
        }
    }

    connect(): Promise<void> {
        // destroy() freed the groups, so reconnecting would bootstrap a FRESH
        // epoch-0 solo group: divergent from every other member and announced
        // with a TOFU registration the relay rejects. Fail closed.
        if (this.destroyed) {
            return Promise.reject(new Error('CollabClient has been destroyed'));
        }
        // Prevent concurrent connection attempts
        if (this.connectPromise) {
            return this.connectPromise;
        }

        // An explicit connect() restarts a stopped client. A group that survived
        // the disconnect is RESUMED (establishGroup skips a slot that still has
        // one); a slot that disconnect() emptied is re-bootstrapped. Only reached
        // on a real connect() call: the reconnect timer checks `stopped` before
        // it gets here, so this cannot revive a stopped client.
        this.stopped = false;
        this.connectionState = 'connecting';
        this.connectPromise = new Promise<void>((resolve, reject) => {
            // Per-attempt flag: has THIS socket reached onopen yet? Every attempt
            // (initial or reconnect) must settle its promise exactly once, so that
            // .finally() clears connectPromise and the dedup guard above won't keep
            // returning a stale, never-settling promise during a transient outage.
            let hasOpened = false;

            try {
                this.ws = new WebSocket(this.config.relayUrl);

                this.ws.onopen = () => {
                    console.log('Connected to relay server');
                    hasOpened = true;
                    this.connectionState = 'connected';
                    this.isInitialConnect = false;
                    // Note: Don't clear connectPromise here - the finally block handles that
                    // to avoid race conditions with concurrent connection attempts

                    // Critical: identify, subscribe and establish the group, or
                    // settle this attempt as failed.
                    if (!this.tryInitialize(reject)) {
                        return;
                    }

                    this.flushMessageQueue();
                    // Arm the stability window instead of refilling the retry
                    // budget here: a successful accept is not proof the
                    // connection is useful, and an accept-then-immediately-drop
                    // relay (shutdown, takeover, eviction) would reconnect
                    // forever because the budget never accumulates toward
                    // maxReconnectAttempts.
                    this.startStabilityTimer();
                    resolve();
                };

                this.ws.onmessage = (event) => {
                    this.handleMessage(event.data);
                };

                this.ws.onerror = (error) => {
                    if (!hasOpened) {
                        // Socket failed before opening. Reject this attempt's promise so
                        // .finally() clears connectPromise (rejection is delegated to
                        // onclose, which follows onerror, to drive the backoff loop).
                        console.error('WebSocket error:', error);
                        reject(error);
                    } else {
                        // Post-open error on a live connection: surface via error callback.
                        this.reportError(
                            'connection',
                            'WebSocket error:',
                            error instanceof Error ? error : new Error('WebSocket error')
                        );
                    }
                };

                this.ws.onclose = () => {
                    console.log('WebSocket closed');
                    if (!hasOpened) {
                        // Attempt closed before opening (initial OR reconnect). Settle this
                        // attempt's promise (no-op if onerror already rejected it) so the
                        // dedup guard is unblocked, then delegate to the backoff scheduler.
                        // Settling does NOT abort the retry chain — handleReconnect() runs
                        // on its own timer independent of this promise.
                        reject(
                            new Error(
                                this.isInitialConnect
                                    ? 'WebSocket closed during initial connection'
                                    : 'WebSocket closed during reconnection'
                            )
                        );
                        if (!this.isInitialConnect) {
                            this.handleReconnect();
                        }
                    } else {
                        this.handleReconnect();
                    }
                };
            } catch (error) {
                reject(error);
            }
        }).finally(() => {
            // Clear promise tracking on completion (success or failure)
            this.connectPromise = null;
        });

        return this.connectPromise;
    }

    private sendIdentify(): boolean {
        return this.send({
            type: 'identify',
            user_id: this.config.userId,
        });
    }

    private subscribe(): boolean {
        const subscribed = this.subscribeTo(this.fileSlot());
        // Vault sync rides the same connection: also subscribe to the manifest
        // doc, whose capability comes from its OWN group (#32).
        if (this.config.vaultSync && this.config.manifestDocId) {
            const manifestSubscribed = this.subscribeTo(this.manifestSlot());
            return subscribed && manifestSubscribed;
        }
        return subscribed;
    }

    /**
     * Subscribe (or re-subscribe) to one group `slot`, presenting a freshly
     * minted capability whenever that group exists (#72).
     *
     * Re-subscribing RE-STATES the relay-side authorization: presenting a
     * capability upgrades the subscription to content-authorized, and a bare
     * subscribe DOWNGRADES it back to handshake-only. So every subscribe after
     * a group exists — including the one every reconnect sends — must carry one.
     *
     * The capability is bound to the slot's LOCALLY-TRUSTED doc id and to this
     * client's own identity, never to a value taken from an inbound frame. The
     * field is omitted (the relay's `Option` default, `None`) while no group
     * exists: a joiner must be subscribed to RECEIVE the Welcome that makes it a
     * member, and only a member can mint.
     */
    private subscribeTo(slot: GroupSlot): boolean {
        const doc = slot.getDoc();
        const capability: unknown = doc
            ? JSON.parse(
                  doc.mint_subscribe_capability(
                      this.config.userId,
                      slot.docId,
                      nowUnix(),
                      CAPABILITY_TTL_SECS
                  )
              )
            : undefined;
        return this.send({ type: 'subscribe', doc_id: slot.docId, capability });
    }

    /**
     * Register this group's FIRST (TOFU) verification anchor with the relay,
     * which is what every capability minted for the document verifies against.
     */
    private registerAnchor(slot: GroupSlot): boolean {
        const doc = slot.getDoc();
        if (!doc) {
            return false;
        }
        return this.registerDocKey(slot.docId, {
            epoch: doc.epoch,
            public_key: doc.subscribe_verifying_key(),
            proof: doc.sign_doc_key_proof(slot.docId),
            // A first registration has no current anchor to prove continuity
            // against; the relay requires a rotation proof only once one exists.
            rotation_proof: new Uint8Array(),
        });
    }

    /** Send one `register_doc_key` frame: a TOFU registration or a rotation. */
    private registerDocKey(docId: string, anchor: DocKeyAnchor): boolean {
        return this.send({
            type: 'register_doc_key',
            doc_id: docId,
            epoch: Number(anchor.epoch),
            public_key: [...anchor.public_key],
            proof: [...anchor.proof],
            rotation_proof: [...anchor.rotation_proof],
        });
    }

    private send(message: object): boolean {
        if (this.ws?.readyState === WebSocket.OPEN) {
            this.ws.send(JSON.stringify(message));
            return true;
        }
        // Queue message instead of silently dropping it, with FIFO eviction at max size
        if (this.messageQueue.length >= this.maxQueueSize) {
            const dropped = this.messageQueue.shift();
            console.warn('[CollabClient] Message queue full, dropping oldest message:', dropped);
        }
        this.messageQueue.push(message);
        return false;
    }

    private flushMessageQueue(): void {
        const failedMessages: object[] = [];
        while (this.messageQueue.length > 0) {
            const message = this.messageQueue.shift();
            if (message && this.ws?.readyState === WebSocket.OPEN) {
                try {
                    this.ws.send(JSON.stringify(message));
                } catch (error) {
                    console.error('Failed to send queued message:', error);
                    failedMessages.push(message);
                }
            }
        }
        // Re-queue failed messages
        this.messageQueue.push(...failedMessages);
    }

    getQueueLength(): number {
        return this.messageQueue.length;
    }

    private handleMessage(data: string): void {
        try {
            // Reject oversized frames BEFORE parsing (see MAX_INBOUND_FRAME_BYTES).
            if (data.length > MAX_INBOUND_FRAME_BYTES) {
                throw new Error(
                    `inbound frame exceeds ${MAX_INBOUND_FRAME_BYTES} bytes (got ${data.length})`
                );
            }
            const message = JSON.parse(data);

            switch (message.type) {
                case 'yrs_update':
                    this.handleYrsUpdate(message as YrsUpdateMessage);
                    break;
                case 'mls_handshake':
                    this.handleMlsHandshake(message as MlsHandshakeMessage);
                    break;
                case 'subscribed':
                    console.log('Subscribed to document:', message.doc_id);
                    break;
                case 'error':
                    this.reportError('sync', 'Server error:', message.message || 'Server error');
                    break;
                default:
                    console.warn(
                        `[CollabClient] Unknown message type received: ${message.type}`,
                        message
                    );
                    break;
            }
        } catch (error) {
            // Prefix preserved so callers can distinguish "the relay sent unparseable
            // JSON" from other 'sync' errors (message asserted on in tests).
            this.reportError(
                'sync',
                'Failed to parse message:',
                error,
                'Failed to parse message: '
            );
        }
    }

    /**
     * Reject a frame routed for a different document before touching any crypto
     * state. The relay is untrusted, so a mismatched doc_id means a misroute or a
     * cross-document replay attempt. (The AEAD/MLS binding of the LOCAL docId
     * downstream is the load-bearing guarantee; this rejects the frame early with
     * a clear error, shared by every inbound frame type that carries a doc_id.)
     */
    private assertDocId(frameType: string, docId: string | undefined): void {
        if (docId !== undefined && docId !== this.config.docId) {
            throw new Error(
                `${frameType} doc_id mismatch: expected ${this.config.docId}, got ${docId}`
            );
        }
    }

    /**
     * Shape and dispatch a `CollabError` to `onErrorCallback`. Defaults to
     * tagging the file group's doc id; pass `docId` to tag the manifest
     * group's instead (#32) — the only thing that ever differed between the
     * two error-reporting call sites.
     */
    private reportError(
        type: CollabError['type'],
        label: string,
        error: unknown,
        messagePrefix = '',
        docId: string | undefined = this.config.docId
    ): void {
        console.error(label, error);
        if (this.onErrorCallback) {
            const collabError: CollabError = {
                type,
                message: `${messagePrefix}${extractErrorMessage(error)}`,
                docId,
                originalError: error instanceof Error ? error : undefined,
            };
            this.onErrorCallback(collabError);
        }
    }

    private handleYrsUpdate(message: YrsUpdateMessage): void {
        try {
            if (!message.encrypted || !Array.isArray(message.encrypted)) {
                throw new Error('Invalid yrs_update message: missing or invalid encrypted field');
            }
            // Manifest updates ride the same channel under their own MLS group:
            // route them before the file-doc guard, which would reject the
            // manifest doc_id as a misroute.
            if (this.isManifestFrame(message.doc_id)) {
                this.handleManifestUpdate(new Uint8Array(message.encrypted));
                return;
            }
            this.assertDocId('yrs_update', message.doc_id);
            // Fail closed: an update that arrives before the MLS group is
            // established cannot be decrypted. Surface it as an error rather than
            // silently dropping it.
            if (this.doc === null) {
                throw new Error('no MLS group established');
            }
            const encrypted = new Uint8Array(message.encrypted);
            // MLS authenticates and decrypts under the group's current epoch.
            this.doc.apply_encrypted_update(encrypted, BigInt(message.epoch ?? 0));

            if (this.onUpdateCallback) {
                this.onUpdateCallback(this.doc.get_content());
            }
        } catch (error) {
            this.reportError('decryption', 'Failed to apply update:', error);
        }
    }

    /**
     * Apply key_package/welcome/commit to one MLS group `slot` — the shared
     * core of the file-group and manifest-group (#32) handshakes, which
     * otherwise differ only in which doc id and which slot they read/write.
     */
    private applyGroupHandshake(
        slot: GroupSlot,
        messageType: MlsHandshakeMessage['message_type'],
        payload: Uint8Array
    ): void {
        switch (messageType) {
            case 'key_package': {
                // Only an owner with an established group answers a key package.
                const doc = slot.getDoc();
                if (this.config.role !== 'owner' || !doc) {
                    return;
                }
                const invite = doc.create_invite(payload);
                // The invite's commit advanced this group's epoch, so the relay's
                // anchor must move with it BEFORE the Welcome goes out: the
                // joiner mints at the new epoch the moment it joins, and a
                // capability whose epoch is ahead of the anchor is REJECTED.
                if (invite.rotation) {
                    this.registerDocKey(slot.docId, invite.rotation);
                }
                // This client's own capability was minted at the previous epoch
                // and no longer verifies; re-present at the new one. Before the
                // Welcome, not after: the rotation just revoked THIS client's own
                // content authorization (the relay gates fan-out on strict epoch
                // equality), and the Welcome is what makes the joiner start
                // sending. Matches the Rust half's register -> present -> welcome
                // order in collab-cli's commands.rs.
                this.subscribeTo(slot);
                // The commit is what carries EXISTING members to the new epoch;
                // forwarding only the Welcome works for exactly two parties and
                // diverges the group at the third. It goes out BEFORE the
                // Welcome: the relay fans every handshake frame out to all other
                // subscribers in order, so the new member sees this too, and only
                // no-ops it while it still has no group (see `case 'commit'`).
                this.send({
                    type: 'mls_handshake',
                    doc_id: slot.docId,
                    payload: [...invite.commit],
                    message_type: 'commit',
                });
                this.send({
                    type: 'mls_handshake',
                    doc_id: slot.docId,
                    payload: [...invite.welcome],
                    message_type: 'welcome',
                });
                break;
            }
            case 'welcome': {
                // Only a joiner that has a pending key package and is NOT already
                // in a group joins. Rejecting when the slot's doc is set prevents
                // an attacker replaying a Welcome to clobber an established group
                // (and the join(invite, null!) throw path when there is no pending).
                const pending = slot.getPending();
                if (this.config.role !== 'joiner' || !pending || slot.getDoc()) {
                    return;
                }
                // Bind the LOCAL group docId, never message.doc_id.
                const invite = WasmInvite.from_welcome(slot.docId, payload);
                // Clear the reference BEFORE calling join(), not after: the
                // generated wasm-bindgen glue destroys `pending`'s handle
                // unconditionally on call entry (pending.__destroy_into_raw()),
                // before the Rust call even runs — so the key package is consumed
                // whether join() succeeds or throws (e.g. a malformed/malicious
                // Welcome from the untrusted relay). If we cleared the slot only
                // on the success line below, a throw here would leave it pointing
                // at the now-dead handle, and every later Welcome (including a
                // legitimate one) would retry join() with that dead handle and
                // throw forever. Clearing in lockstep with the consuming call makes
                // a failed join fail closed exactly like the documented
                // socket-drops-mid-handshake case: un-joined, no plaintext, a fresh
                // session is required to retry.
                slot.setPending(null);
                slot.setDoc(WasmEncryptedDocument.join(invite, pending));
                // A member only now, so able to mint only now: re-subscribing with
                // a capability upgrades this connection from handshake-only to
                // content-authorized. Without it the relay withholds every
                // yrs_update (#72) — this ordering is the deadlock the issue exists
                // to fix, so it must stay AFTER the join, never before.
                this.subscribeTo(slot);
                break;
            }
            case 'commit': {
                // No group yet: this is the add-commit that admits THIS client,
                // fanned out to every subscriber ahead of its own Welcome. There
                // is nothing to advance and nothing to re-present — a bare
                // subscribe here would only downgrade what is already
                // handshake-only.
                const doc = slot.getDoc();
                if (!doc) {
                    return;
                }
                // An existing member follows the owner's commit to the new epoch.
                // The rotation it returns is DELIBERATELY dropped: the owner
                // already registered the identical anchor for this epoch, and
                // only one registration can win. The relay verifies continuity
                // under the CURRENT anchor key and then demands a strictly higher
                // epoch, so a second registration of the same rotation is
                // rejected twice over (crates/collab-relay/src/relay.rs,
                // `handle_register_doc_key`). Mirrors the Rust choreography in
                // `three_real_members` (tests/e2e-tests/tests/subscribe_authz.rs),
                // where only the owner registers.
                doc.process_commit(payload);
                // This client's capability was minted at the previous epoch and
                // no longer verifies against the rotated anchor; re-present at
                // the new one or the relay withholds every yrs_update (#72).
                this.subscribeTo(slot);
                break;
            }
            default:
                console.warn(`[CollabClient] Unknown mls_handshake message_type: ${messageType}`);
                break;
        }
    }

    /**
     * Handle the MLS handshake wire protocol (#51), routing to the file group
     * or the manifest group (#32) — the two groups share the exact same
     * key_package/welcome/commit protocol via `applyGroupHandshake`.
     */
    private handleMlsHandshake(message: MlsHandshakeMessage): void {
        try {
            if (!message.payload || !Array.isArray(message.payload)) {
                throw new Error('Invalid mls_handshake message: missing or invalid payload');
            }
            const payload = new Uint8Array(message.payload);
            // Manifest-group handshake rides the same channel; route it before the
            // file-doc guard (which would reject the manifest doc_id as a misroute).
            if (this.isManifestFrame(message.doc_id)) {
                try {
                    this.applyGroupHandshake(this.manifestSlot(), message.message_type, payload);
                } catch (error) {
                    this.reportError(
                        'sync',
                        'Failed to process manifest handshake:',
                        error,
                        '',
                        this.config.manifestDocId
                    );
                }
                return;
            }
            this.assertDocId('mls_handshake', message.doc_id);
            this.applyGroupHandshake(this.fileSlot(), message.message_type, payload);
        } catch (error) {
            this.reportError('sync', 'Failed to process MLS handshake:', error);
        }
    }

    /**
     * Decrypt and apply a remote manifest update (#32).
     *
     * Decryption is authenticated by the manifest MLS group: a ciphertext bound
     * to any other group (a file doc, a stale group) fails here and never
     * reaches the manifest. Newly-announced paths are subscribed to and surfaced
     * via onManifestPaths.
     */
    private handleManifestUpdate(encrypted: Uint8Array): void {
        try {
            if (this.manifestDoc === null) {
                // Fail closed: an update before the manifest group is established
                // cannot be decrypted.
                throw new Error('no manifest MLS group established');
            }
            const plaintext = this.manifestDoc.decrypt_bytes(encrypted);
            const newPaths = this.config.vaultSync!.apply_remote_manifest(plaintext);
            for (const path of newPaths) {
                // Capability-less on purpose: a newly-announced path has no MLS
                // group on this client yet, so there is nothing to mint from.
                // Under subscribe authorization these subscriptions are
                // handshake-only and receive no content until a group for the
                // path is established (#72 follow-up).
                this.send({ type: 'subscribe', doc_id: path });
            }
            if (this.onManifestPathsCallback) {
                void this.onManifestPathsCallback(newPaths);
            }
        } catch (error) {
            this.reportError(
                'decryption',
                'Failed to apply manifest update:',
                error,
                '',
                this.config.manifestDocId
            );
        }
    }

    /** Broadcast an encrypted CRDT op as a `yrs_update` frame for `docId`. */
    private sendYrsUpdate(docId: string, op: WasmEncryptedOp): boolean {
        return this.send({
            type: 'yrs_update',
            doc_id: docId,
            encrypted: [...op.ciphertext],
            epoch: Number(op.epoch),
        });
    }

    /**
     * Encrypt and broadcast a local manifest update (#32) under the manifest MLS
     * group's current epoch. Fails closed (returns false) until that group is
     * established. Queues like any other frame when disconnected.
     */
    sendManifestUpdate(update: Uint8Array): boolean {
        const { vaultSync, manifestDocId } = this.config;
        if (!vaultSync || !manifestDocId) {
            console.warn('[CollabClient] sendManifestUpdate called without vault sync configured');
            return false;
        }
        if (this.manifestDoc === null) {
            return false;
        }
        try {
            return this.sendYrsUpdate(manifestDocId, this.manifestDoc.encrypt_bytes(update));
        } catch (error) {
            this.reportError('sync', 'Failed to send manifest update:', error, '', manifestDocId);
            return false;
        }
    }

    onManifestPaths(callback: ManifestPathsCallback): void {
        this.onManifestPathsCallback = callback;
    }

    /**
     * Arm the stability window. Only once a connection has stayed up for
     * `minStableConnectionMs` is the retry budget refilled — the TS analogue of
     * the Rust `on_stable_connection` (crates/collab-core/src/connection.rs:302),
     * driven there by MIN_STABLE_CONNECTION in the CLI. Refilling on `onopen`
     * alone is the accept-then-drop bug: the budget would never accumulate and
     * an accept-then-immediately-drop relay would be retried forever.
     *
     * The timer is cleared on every drop (handleReconnect) and by disconnect(),
     * so a refill can never outlive the connection that earned it.
     */
    private startStabilityTimer(): void {
        this.clearStabilityTimer();
        this.stabilityTimer = setTimeout(() => {
            this.stabilityTimer = null;
            // Never hand a budget back to a client the user explicitly stopped.
            if (this.stopped) {
                return;
            }
            this.reconnectAttempts = 0;
        }, this.config.minStableConnectionMs ?? DEFAULT_MIN_STABLE_CONNECTION_MS);
    }

    private clearStabilityTimer(): void {
        if (this.stabilityTimer) {
            clearTimeout(this.stabilityTimer);
            this.stabilityTimer = null;
        }
    }

    private handleReconnect(): void {
        // A stopped client runs none of this: disconnect() closes the socket
        // itself, so the resulting onclose must not schedule a retry, flip the
        // state to 'reconnecting', or report 'max_retries_exceeded' to a user who
        // asked to stop. disconnect() has already cleared both timers.
        if (this.stopped) {
            return;
        }
        // Clear any existing timer to prevent old timers from interfering
        if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = null;
        }
        // The connection this was armed for is gone, so it never proved stable.
        // Every drop routes through here, which is what keeps a pending refill
        // from surviving the socket it belonged to.
        this.clearStabilityTimer();

        // Pure configuration, read fresh and never written: the client's stopped
        // state is tracked separately by `this.stopped`.
        const maxAttempts = this.config.maxReconnectAttempts ?? DEFAULT_MAX_RECONNECT_ATTEMPTS;
        if (this.reconnectAttempts < maxAttempts) {
            this.connectionState = 'reconnecting';
            this.reconnectAttempts++;
            // Cap the backoff, mirroring `delay.min(self.max_delay)` in
            // crates/collab-core/src/connection.rs:153.
            const delay = Math.min(
                this.reconnectDelay * Math.pow(2, this.reconnectAttempts - 1),
                this.config.maxReconnectDelayMs ?? DEFAULT_MAX_RECONNECT_DELAY_MS
            );
            console.log(`Reconnecting in ${delay}ms (attempt ${this.reconnectAttempts})`);
            this.reconnectTimer = setTimeout(() => {
                // Check if disconnect() was called while waiting
                if (this.stopped) {
                    return;
                }
                this.connect().catch((error) => {
                    this.reportError(
                        'connection',
                        'Reconnect failed:',
                        error instanceof Error ? error : new Error('Reconnection failed')
                    );
                });
            }, delay);
        } else {
            this.connectionState = 'disconnected';
            // reconnectTimer is already null here (cleared at entry, only reassigned
            // in the branch above), so no timer cleanup is needed.
            if (this.onDisconnectCallback) {
                this.onDisconnectCallback('max_retries_exceeded');
            }
        }
    }

    /**
     * Apply a minimal text diff between the current CRDT text and the new text.
     * This avoids clearing and reinserting the entire document, which is
     * critical for proper CRDT collaborative behavior.
     */
    private applyTextDiff(oldText: string, newText: string): void {
        if (oldText === newText) {
            return;
        }

        const oldLen = oldText.length;
        const newLen = newText.length;

        // Find common prefix length
        let prefixLen = 0;
        const maxPrefix = Math.min(oldLen, newLen);
        while (prefixLen < maxPrefix && oldText.charAt(prefixLen) === newText.charAt(prefixLen)) {
            prefixLen++;
        }

        // Find common suffix length (after the prefix)
        let oldEnd = oldLen;
        let newEnd = newLen;
        while (
            oldEnd > prefixLen &&
            newEnd > prefixLen &&
            oldText.charAt(oldEnd - 1) === newText.charAt(newEnd - 1)
        ) {
            oldEnd--;
            newEnd--;
        }

        // Calculate what to delete and insert
        const deleteLen = oldEnd - prefixLen;
        const insertText = newText.slice(prefixLen, newEnd);

        // Apply minimal operations
        if (deleteLen > 0) {
            this.doc?.delete(prefixLen, deleteLen);
        }
        if (insertText.length > 0) {
            this.doc?.insert(prefixLen, insertText);
        }
    }

    sendUpdate(text: string): boolean {
        // Fail-closed guard (replaces the old all-zeros-key guard): without an
        // established MLS group there is no key to encrypt under, so send NOTHING
        // rather than falling back to a plaintext path.
        if (this.doc === null) {
            return false;
        }
        try {
            const currentText = this.doc.get_content();

            if (text !== currentText) {
                // Apply minimal diff instead of clearing and reinserting
                this.applyTextDiff(currentText, text);
            }

            // MLS encrypts under the group's current epoch; the op carries both.
            return this.sendYrsUpdate(this.config.docId, this.doc.get_encrypted_update());
        } catch (error) {
            this.reportError('sync', 'Failed to send update:', error);
            return false;
        }
    }

    onUpdate(callback: UpdateCallback): void {
        this.onUpdateCallback = callback;
    }

    onDisconnect(callback: DisconnectCallback): void {
        this.onDisconnectCallback = callback;
    }

    onError(callback: ErrorCallback): void {
        this.onErrorCallback = callback;
    }

    getConnectionState(): ConnectionState {
        return this.connectionState;
    }

    getText(): string {
        return this.doc?.get_content() ?? '';
    }

    /**
     * Free and null out one group slot's doc + pending handles. Shared by
     * disconnect() for the file group and the manifest group (#32) — same
     * free/null pattern GroupSlot already abstracts for bootstrap/handshake.
     *
     * Do NOT call this after a successful join() — join() consumes the
     * pending handle by value, so freeing it here too would double-free.
     */
    private freeSlot(slot: GroupSlot): void {
        slot.getDoc()?.free();
        slot.setDoc(null);
        slot.getPending()?.free();
        slot.setPending(null);
    }

    disconnect(): void {
        this.stopped = true; // Prevent reconnection until connect() is called again
        this.connectPromise = null; // Clear any pending connection promise
        this.connectionState = 'disconnected';
        if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = null;
        }
        // A stability timer that outlived disconnect() would fire later and
        // resurrect the retry budget of a session the user explicitly stopped.
        this.clearStabilityTimer();
        this.ws?.close();
        this.ws = null;
        // An ESTABLISHED MLS group outlives the socket and must survive an
        // explicit stop. Freeing it here made the next connect() build a FRESH
        // epoch-0 group: silently divergent from every other member, and
        // announced with a TOFU register_doc_key the relay rejects — an anchor
        // for the document already exists, so it demands a rotation-continuity
        // proof — followed by an epoch-0 capability it rejects as Unauthorized.
        //
        // Only a slot with NO group is torn down. That releases a still-
        // unconsumed key package (a joiner that never got its Welcome) and lets
        // the next connect() re-bootstrap that slot from scratch; see freeSlot's
        // doc comment for the double-free hazard. destroy() releases the groups.
        // Emptying that slot is also what tells the next connect() to
        // re-bootstrap it: establishGroup() bootstraps exactly the slots with
        // neither a doc nor a pending.
        [this.fileSlot(), this.manifestSlot()]
            .filter((slot) => !slot.getDoc())
            .forEach((slot) => this.freeSlot(slot));
    }

    /**
     * End the session: stop the client and release its MLS groups.
     *
     * `disconnect()` is a pause — it keeps an established group so a later
     * `connect()` RESUMES it rather than re-creating one. `destroy()` is the
     * end, freeing the wasm handles `disconnect()` deliberately holds. The
     * client is not reusable afterwards — `connect()` rejects.
     */
    destroy(): void {
        this.disconnect();
        this.freeSlot(this.fileSlot());
        this.freeSlot(this.manifestSlot());
        this.destroyed = true;
    }
}
