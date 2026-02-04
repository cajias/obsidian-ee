# Security Threat Analysis (SOTA)

State-of-the-art analysis of security risks when adopting P2P transport for MLS-encrypted collaborative editing.

## Risk Matrix Overview

| Risk | y-webrtc | y-libp2p | Severity | Page |
|------|----------|----------|----------|------|
| MLS Epoch Desync | High | High | **Critical** | [Details](./mls-epoch-desync.md) |
| Welcome Message Loss | Medium | Medium | High | [Details](./welcome-message-loss.md) |
| Split-Brain Groups | Medium | Low | High | [Details](./split-brain-groups.md) |
| IP Address Leakage | High | Medium | Medium | [Details](./ip-address-leakage.md) |
| Eclipse Attacks | Low | Medium | High | [Details](./eclipse-attacks.md) |
| Sybil Attacks | Low | High | High | [Details](./sybil-attacks.md) |

## Risk Categories

### Protocol-Level Risks (MLS)

These risks affect the correctness of MLS encryption:

- **[MLS Epoch Desync](./mls-epoch-desync.md)** - Out-of-order commits break decryption
- **[Welcome Message Loss](./welcome-message-loss.md)** - New members cannot join groups
- **[Split-Brain Groups](./split-brain-groups.md)** - Network partitions cause divergent state

### Network-Level Risks (P2P)

These risks are inherent to P2P networking:

- **[IP Address Leakage](./ip-address-leakage.md)** - Privacy loss through direct connections
- **[Eclipse Attacks](./eclipse-attacks.md)** - Malicious peer isolation
- **[Sybil Attacks](./sybil-attacks.md)** - Fake identity flooding

## Severity Definitions

| Level | Definition |
|-------|------------|
| **Critical** | System becomes unusable, data loss possible |
| **High** | Major functionality impaired, security degraded |
| **Medium** | Functionality limited, privacy concerns |
| **Low** | Minor impact, workarounds available |

## Library Comparison

```mermaid
quadrantChart
    title Security vs Scalability
    x-axis Low Security --> High Security
    y-axis Low Scalability --> High Scalability
    quadrant-1 Ideal
    quadrant-2 Secure but Limited
    quadrant-3 Avoid
    quadrant-4 Scalable but Risky
    y-webrtc: [0.6, 0.3]
    y-libp2p: [0.4, 0.8]
    Relay (v1): [0.8, 0.5]
    Hybrid: [0.75, 0.75]
```

## Recommendation

Based on this analysis, we recommend a **hybrid architecture**:

- **Control Plane (Relay)**: MLS handshakes requiring ordered delivery
- **Data Plane (P2P)**: Encrypted CRDT updates tolerant of disorder

See [P2P Architecture](../p2p-architecture.md) for implementation details.
