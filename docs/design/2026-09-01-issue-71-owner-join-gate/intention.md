# Intent — Owner-side join gate (#71)

Issue [#71](https://github.com/cajias/obsidian-ee/issues/71) · P1 · `track-security` · not started

## What we intend to change

An owner must authorize a requester before it becomes a decrypting member of a
document's MLS group.

## Why

Today the owner admits everyone. `applyGroupHandshake`, case `'key_package'`
(`plugins/obsidian-ee/src/collab-client.ts:663-677`), answers every inbound key package on
its document channel with a Welcome — no allowlist, no invite token, no confirmation. Anyone
who can reach the relay and knows a `doc_id` can decrypt every subsequent update.
Relay-reachability equals membership. Disclosed as a deliberate non-protection in
`docs/security.md:49-60`; this closes it.

MLS is not broken — the relay learns nothing, and a never-welcomed non-member cannot
decrypt. The unguarded decision is *who becomes a member*.

## Scope

In: the plugin's automatic key-package responder, and whatever authorization input it needs.

Out: the CLI invite path (`crates/collab-cli/src/commands.rs:122` — operator-typed, already
human-gated), relay-side subscribe/content authz (#29, #72), snapshot slot binding (#76),
revocation after admission (#31, landed).

## Done when

An unauthorized key package provably gets no Welcome and does not extend the group, proven
by a negative test that is RED before the change and GREEN after; a legitimate join still
works; `docs/security.md` states the new admission model and its residual weaknesses.
