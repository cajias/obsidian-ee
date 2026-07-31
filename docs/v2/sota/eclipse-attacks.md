# Eclipse Attacks in P2P Networks

This document provides a comprehensive analysis of eclipse attacks in peer-to-peer networks, with specific focus on their implications for collaborative editing systems like obsidian-ee.

## Overview

### What is an Eclipse Attack?

An **eclipse attack** is a network-level attack where an adversary monopolizes all incoming and outgoing connections of a victim node, effectively isolating it from the legitimate network. The attacker "eclipses" the victim's view of the network, controlling all information the victim receives and sends.

```mermaid
flowchart TB
    subgraph "Normal Network View"
        direction TB
        V1[Victim] <--> H1[Honest Peer 1]
        V1 <--> H2[Honest Peer 2]
        V1 <--> H3[Honest Peer 3]
        H1 <--> H2
        H2 <--> H3
    end

    subgraph "Eclipsed Network View"
        direction TB
        V2[Victim] <--> M1[Malicious 1]
        V2 <--> M2[Malicious 2]
        V2 <--> M3[Malicious 3]
        M1 -.->|"Controlled"| A[Attacker]
        M2 -.->|"Controlled"| A
        M3 -.->|"Controlled"| A

        H4[Honest Peer 1]
        H5[Honest Peer 2]
        H4 <--> H5

        V2 x--x H4
        V2 x--x H5
    end
```

### Why Eclipse Attacks Matter for P2P

In decentralized systems, eclipse attacks undermine fundamental security assumptions:

| Assumption | Reality Under Eclipse |
|------------|----------------------|
| Peers provide diverse, honest views | All views controlled by attacker |
| Gossip reaches all network participants | Messages censored or delayed |
| Consensus reflects network majority | Victim sees attacker's "consensus" |
| Updates propagate reliably | Selective message filtering |

For collaborative editing systems, eclipse attacks can cause:
- **Document divergence** - Victim's edits never reach other collaborators
- **Stale state attacks** - Victim works on outdated document versions
- **Partition manipulation** - Artificial network splits for conflict exploitation

## Technical Background

### Peer Discovery Mechanisms

P2P networks rely on various discovery mechanisms, each with different vulnerability profiles:

#### Distributed Hash Table (DHT)

DHTs like Kademlia map peer IDs and content to locations in a virtual address space using XOR distance metrics.

```mermaid
flowchart LR
    subgraph "Kademlia Routing Table"
        direction TB
        B0[k-bucket 0<br/>Distance: 2^0]
        B1[k-bucket 1<br/>Distance: 2^1]
        B2[k-bucket 2<br/>Distance: 2^2]
        BN[k-bucket n<br/>Distance: 2^n]

        B0 --> B1 --> B2 --> BN
    end

    Node[Local Node] --> B0

    subgraph "Attack Surface"
        M1[Malicious nodes<br/>fill k-buckets]
        M2[Strategic ID<br/>generation]
    end

    M1 -.-> B0
    M1 -.-> B1
    M2 -.-> BN
```

**Vulnerability:** Attackers can generate node IDs strategically placed near target keys, polluting the victim's routing table.

#### Bootstrap Nodes

Initial network entry relies on well-known bootstrap nodes:

```
1. New node contacts bootstrap node
2. Bootstrap provides initial peer list
3. Node populates routing table from these peers
4. DHT queries expand network knowledge
```

**Vulnerability:** Compromised or malicious bootstrap nodes can provide exclusively attacker-controlled peers.

#### mDNS (Multicast DNS)

Local network discovery via multicast announcements.

**Vulnerability:** Attackers on the same LAN can flood mDNS responses, appearing as multiple legitimate peers.

### Routing Tables and Neighbor Selection

#### Connection Limits

Most P2P implementations impose connection limits:

```rust
// Typical libp2p configuration
struct ConnectionLimits {
    max_established_incoming: u32,  // e.g., 128
    max_established_outgoing: u32,  // e.g., 128
    max_pending_incoming: u32,      // e.g., 32
    max_pending_outgoing: u32,      // e.g., 32
}
```

**Attack implication:** Finite connection slots mean attackers only need to fill these slots to achieve eclipse.

#### Peer Selection Algorithms

Networks use various strategies to select which peers to maintain connections with:

| Strategy | Description | Eclipse Resistance |
|----------|-------------|-------------------|
| Random | Uniform random selection | Low - predictable |
| Closest (DHT) | XOR distance to local ID | Medium - ID manipulation |
| Reputation-based | Historical behavior scoring | High - requires track record |
| Diverse | Geographic/topological diversity | High - harder to simulate |

### GossipSub Mesh Construction

GossipSub (used in Ethereum 2.0, IPFS, and libp2p) maintains topic-specific mesh overlays:

```mermaid
flowchart TB
    subgraph "GossipSub Mesh for Topic X"
        direction TB
        N1[Node 1]
        N2[Node 2]
        N3[Node 3]
        N4[Node 4]
        N5[Node 5]

        N1 <-->|"mesh"| N2
        N1 <-->|"mesh"| N3
        N2 <-->|"mesh"| N4
        N3 <-->|"mesh"| N5
        N4 <-->|"mesh"| N5

        N1 -.->|"gossip"| N4
        N2 -.->|"gossip"| N5
    end

    subgraph "Parameters"
        D[D = 6<br/>Target degree]
        DLo[D_lo = 4<br/>Min degree]
        DHi[D_hi = 12<br/>Max degree]
    end
```

**Mesh parameters:**
- **D (degree):** Target number of mesh peers (default: 6)
- **D_lo:** Minimum mesh peers before GRAFT requests (default: 4)
- **D_hi:** Maximum mesh peers before PRUNE (default: 12)
- **D_lazy:** Gossip-only peers for redundancy (default: 6)

## Risk Assessment by Library

### y-webrtc: LOW Risk

y-webrtc uses WebRTC for peer-to-peer connections with a centralized signaling server.

```mermaid
flowchart TB
    subgraph "y-webrtc Architecture"
        S[Signaling Server]

        A[Peer A] <-->|"SDP Offer/Answer"| S
        B[Peer B] <-->|"SDP Offer/Answer"| S
        C[Peer C] <-->|"SDP Offer/Answer"| S

        A <-->|"WebRTC DataChannel"| B
        A <-->|"WebRTC DataChannel"| C
        B <-->|"WebRTC DataChannel"| C
    end

    style S fill:#90EE90
```

**Risk Factors:**

| Factor | Assessment | Notes |
|--------|------------|-------|
| Peer Discovery | Centralized (signaling) | Server controls peer introduction |
| Connection Topology | Full mesh | Limited scalability reduces attack surface |
| Bootstrap Trust | Single server | Server compromise = network compromise |
| ID Generation | Server-controlled | Cannot manipulate peer IDs |

**Why Lower Risk:**
1. Signaling server acts as trusted introducer
2. Room-based isolation limits peer visibility
3. Small group sizes (10-50 peers) make full mesh feasible
4. No DHT means no routing table poisoning

**Residual Risks:**
- Signaling server compromise provides full eclipse capability
- STUN/TURN server manipulation can affect connectivity
- Same-room attackers can still flood connections

### y-libp2p / GossipSub: MEDIUM-HIGH Risk

libp2p provides a full P2P stack with DHT-based discovery and gossip-based message propagation.

```mermaid
flowchart TB
    subgraph "libp2p Attack Surfaces"
        direction LR

        subgraph "Discovery Layer"
            DHT[Kademlia DHT]
            Boot[Bootstrap Nodes]
            MDNS[mDNS]
        end

        subgraph "Routing Layer"
            RT[Routing Table]
            Conn[Connection Manager]
        end

        subgraph "Gossip Layer"
            Mesh[GossipSub Mesh]
            Score[Peer Scoring]
        end

        DHT -->|"Poisoning"| RT
        Boot -->|"Malicious list"| RT
        MDNS -->|"Flooding"| RT
        RT -->|"Malicious peers"| Conn
        Conn -->|"Eclipse"| Mesh
    end

    style DHT fill:#FFB6C1
    style Boot fill:#FFB6C1
    style Mesh fill:#FFB6C1
```

**Risk Factors:**

| Factor | Assessment | Notes |
|--------|------------|-------|
| Peer Discovery | Decentralized (DHT) | Vulnerable to poisoning |
| Connection Topology | Sparse mesh | Fewer connections = easier eclipse |
| Bootstrap Trust | Multiple nodes | Diversity helps, but all could be compromised |
| ID Generation | Self-generated | Strategic ID placement possible |
| Routing | Distance-based | Predictable neighbor selection |

**Attack Vectors:**

1. **DHT Poisoning:** Flood DHT with malicious peer records for target keys
2. **Sybil Amplification:** Generate many node IDs near victim's ID
3. **Bootstrap Compromise:** Control initial peer list provided to new nodes
4. **Connection Exhaustion:** Fill victim's connection slots before honest peers
5. **Strategic Positioning:** Place malicious nodes along routing paths

## Attack Scenario: Isolating a Victim Node

The following diagram illustrates a complete eclipse attack against a node using libp2p:

```mermaid
sequenceDiagram
    participant V as Victim Node
    participant A as Attacker Controller
    participant M1 as Malicious Node 1
    participant M2 as Malicious Node 2
    participant M3 as Malicious Node 3
    participant H as Honest Network

    Note over A: Phase 1: Reconnaissance
    A->>V: Probe network position
    A->>A: Generate strategic node IDs
    A->>M1: Deploy near victim's ID
    A->>M2: Deploy near victim's ID
    A->>M3: Deploy near victim's ID

    Note over A: Phase 2: DHT Poisoning
    M1->>V: DHT PUT (malicious peer records)
    M2->>V: DHT PUT (malicious peer records)
    M3->>V: DHT PUT (malicious peer records)

    Note over A: Phase 3: Connection Flooding
    M1->>V: Connection request (fills slot)
    M2->>V: Connection request (fills slot)
    M3->>V: Connection request (fills slot)
    Note over V: Connection limit reached

    H->>V: Connection request
    V-->>H: Rejected (no slots)

    Note over A: Phase 4: Eclipse Achieved

    rect rgb(255, 230, 230)
        Note over V,H: Victim isolated from honest network

        V->>M1: Document update
        M1->>A: Forward to controller
        A->>A: Censor/Delay/Modify

        H->>H: Honest network continues
        Note over V: Victim diverges from network state
    end

    Note over A: Phase 5: Exploitation
    A->>M1: Send stale/malicious updates
    M1->>V: Deliver manipulated content
    Note over V: Victim accepts attacker's "truth"
```

### Attack Timeline

```mermaid
gantt
    title Eclipse Attack Progression
    dateFormat X
    axisFormat %s

    section Preparation
    Reconnaissance           :a1, 0, 10
    ID Generation            :a2, 5, 15
    Node Deployment          :a3, 15, 25

    section Execution
    DHT Poisoning           :b1, 25, 40
    Connection Flooding      :b2, 35, 50
    Table Eviction          :b3, 45, 60

    section Eclipse
    Full Isolation          :crit, c1, 60, 100

    section Exploitation
    Message Censorship      :c2, 65, 100
    State Manipulation      :c3, 70, 100
```

### Technical Requirements for Attack

To successfully eclipse a victim, an attacker typically needs:

| Requirement | Typical Value | Notes |
|-------------|---------------|-------|
| Malicious nodes | 8-20 | Depends on k-bucket size |
| Strategic IDs | 50-100 | For DHT proximity |
| Time to execute | 10-60 minutes | Depends on routing table refresh |
| Bandwidth | Moderate | DHT operations are lightweight |
| Victim online time | Continuous | Easier if victim restarts |

## Impact Analysis

### Message Censorship

The attacker can selectively filter messages:

```mermaid
flowchart LR
    subgraph "Message Types"
        direction TB
        CRDT[CRDT Updates]
        MLS[MLS Handshakes]
        Meta[Membership Changes]
    end

    subgraph "Attacker Actions"
        direction TB
        Drop[Drop silently]
        Delay[Delay significantly]
        Reorder[Reorder delivery]
        Inject[Inject fake messages]
    end

    CRDT -->|"Censor"| Drop
    CRDT -->|"Slow"| Delay
    MLS -->|"Block"| Drop
    Meta -->|"Manipulate"| Inject

    subgraph "Impact"
        direction TB
        I1[Document divergence]
        I2[Epoch desync]
        I3[Group partition]
    end

    Drop --> I1
    Drop --> I2
    Inject --> I3
```

### Delayed Updates

| Delay Duration | Impact on Collaborative Editing |
|----------------|--------------------------------|
| < 1 second | Unnoticeable |
| 1-10 seconds | Degraded real-time experience |
| 10-60 seconds | Visible lag, conflict likelihood increases |
| > 60 seconds | Effective partition, major divergence |

### Partition from Honest Peers

Consequences of network partition:

```mermaid
flowchart TB
    subgraph "Before Eclipse"
        V1[Victim] --> D1[Document v1.0]
        H1[Honest A] --> D1
        H2[Honest B] --> D1
    end

    subgraph "During Eclipse (Divergence)"
        V2[Victim] --> D2[Document v1.1a<br/>Victim's isolated edits]
        H3[Honest A] --> D3[Document v1.1b<br/>Network's collaborative edits]
        H4[Honest B] --> D3
    end

    subgraph "After Eclipse (Resolution)"
        V3[Victim] --> D4[Document v1.2<br/>CRDT Merge Required]
        H5[Honest A] --> D4
        H6[Honest B] --> D4

        D4 -->|"May contain"| C1[Conflicts]
        D4 -->|"May lose"| C2[Causal ordering]
        D4 -->|"May have"| C3[Duplicate content]
    end
```

### Impact on MLS Groups

Eclipse attacks have severe implications for MLS-based encryption:

| MLS Operation | Eclipse Impact | Severity |
|---------------|----------------|----------|
| Commit (epoch advance) | Victim misses epoch, cannot decrypt | **Critical** |
| Welcome (new member) | New member unreachable | High |
| Update (key rotation) | Forward secrecy compromised | High |
| Remove (member eviction) | Victim unaware of removal | Medium |
| Application message | Content censorship | Medium |

## Mitigations

### 1. Peer Scoring

GossipSub v1.1 introduced peer scoring to resist eclipse and Sybil attacks:

```rust
// GossipSub peer scoring configuration
pub struct PeerScoreParams {
    /// Score thresholds
    pub gossip_threshold: f64,      // Below this: no gossip
    pub publish_threshold: f64,     // Below this: no publish
    pub graylist_threshold: f64,    // Below this: ignore
    pub accept_px_threshold: f64,   // Above this: accept peer exchange
    pub opportunistic_graft_threshold: f64,

    /// Decay parameters
    pub decay_interval: Duration,
    pub decay_to_zero: f64,
    pub retain_score: Duration,

    /// Topic-specific scoring
    pub topics: HashMap<TopicHash, TopicScoreParams>,

    /// IP colocation penalty
    pub ip_colocation_factor_weight: f64,
    pub ip_colocation_factor_threshold: usize,
}

pub struct TopicScoreParams {
    pub topic_weight: f64,

    /// P1: Time in mesh
    pub time_in_mesh_weight: f64,
    pub time_in_mesh_quantum: Duration,
    pub time_in_mesh_cap: f64,

    /// P2: First message deliveries
    pub first_message_deliveries_weight: f64,
    pub first_message_deliveries_decay: f64,
    pub first_message_deliveries_cap: f64,

    /// P3: Mesh message delivery rate
    pub mesh_message_deliveries_weight: f64,
    pub mesh_message_deliveries_decay: f64,
    pub mesh_message_deliveries_threshold: f64,

    /// P3b: Mesh failure penalty
    pub mesh_failure_penalty_weight: f64,
    pub mesh_failure_penalty_decay: f64,

    /// P4: Invalid messages
    pub invalid_message_deliveries_weight: f64,
    pub invalid_message_deliveries_decay: f64,
}
```

**Scoring factors:**

| Factor | Weight | Description |
|--------|--------|-------------|
| Time in mesh (P1) | Positive | Rewards long-term peers |
| First deliveries (P2) | Positive | Rewards message originators |
| Mesh delivery (P3) | Positive | Rewards reliable delivery |
| Mesh failures (P3b) | Negative | Penalizes delivery failures |
| Invalid messages (P4) | Negative | Penalizes protocol violations |
| IP colocation | Negative | Penalizes many peers from same IP |

### 2. Diverse Bootstrap Nodes

```mermaid
flowchart TB
    subgraph "Vulnerable: Single Bootstrap"
        N1[New Node] --> B1[Bootstrap Node]
        B1 -->|"All malicious?"| M[Malicious Peers]
    end

    subgraph "Resilient: Diverse Bootstrap"
        N2[New Node] --> B2[Bootstrap 1<br/>Org A]
        N2 --> B3[Bootstrap 2<br/>Org B]
        N2 --> B4[Bootstrap 3<br/>Org C]

        B2 --> H1[Honest Peers]
        B3 --> H2[Honest Peers]
        B4 --> H3[Honest Peers]

        Note1[Different operators]
        Note2[Different jurisdictions]
        Note3[Different infrastructure]
    end

    style B1 fill:#FFB6C1
    style B2 fill:#90EE90
    style B3 fill:#90EE90
    style B4 fill:#90EE90
```

**Bootstrap diversity requirements:**

| Diversity Axis | Rationale |
|----------------|-----------|
| Organizational | Different operators less likely to collude |
| Geographic | Jurisdiction diversity, latency diversity |
| Infrastructure | Different cloud providers, different failure modes |
| Protocol | Mix of DHT, mDNS, hardcoded improves resilience |

### 3. Connection Limits and Slot Reservation

```rust
// Connection management for eclipse resistance
pub struct EclipseResistantConnectionManager {
    /// Total connection limits
    max_connections: usize,

    /// Reserved slots for different peer classes
    reserved_bootstrap: usize,    // Always maintain bootstrap connections
    reserved_long_term: usize,    // Slots for established peers
    reserved_diverse: usize,      // Slots for topologically diverse peers

    /// Churn protection
    min_connection_duration: Duration,
    reconnection_backoff: ExponentialBackoff,

    /// Diversity requirements
    max_per_subnet: usize,        // /24 subnet limit
    max_per_asn: usize,           // AS number limit
}

impl EclipseResistantConnectionManager {
    pub fn should_accept(&self, peer: &PeerId, addr: &Multiaddr) -> bool {
        // Check connection limits
        if self.current_connections() >= self.max_connections {
            return false;
        }

        // Check subnet diversity
        if self.peers_in_subnet(addr) >= self.max_per_subnet {
            return false;
        }

        // Check ASN diversity
        if self.peers_in_asn(addr) >= self.max_per_asn {
            return false;
        }

        // Prefer peers with good scores
        if self.has_low_score(peer) {
            return false;
        }

        true
    }
}
```

### 4. Reputation Systems

```mermaid
flowchart TB
    subgraph "Reputation Sources"
        Local[Local Observations]
        Global[Network Attestations]
        Historic[Historical Behavior]
    end

    subgraph "Reputation Factors"
        Uptime[Uptime consistency]
        Delivery[Message delivery rate]
        Validity[Protocol compliance]
        Age[Account/Node age]
    end

    subgraph "Actions Based on Reputation"
        Accept[Accept connections]
        Prefer[Prefer in mesh]
        Reject[Reject connections]
        Ban[Ban peer]
    end

    Local --> Uptime
    Local --> Delivery
    Local --> Validity
    Global --> Validity
    Historic --> Age
    Historic --> Uptime

    Uptime -->|"High"| Accept
    Delivery -->|"High"| Prefer
    Validity -->|"Low"| Reject
    Age -->|"New + suspicious"| Reject
    Validity -->|"Violations"| Ban
```

### 5. Additional Mitigations

| Mitigation | Implementation | Effectiveness |
|------------|----------------|---------------|
| **Random peer selection** | Uniform random from candidate set | Prevents targeted filling |
| **Slot aging** | Older connections harder to evict | Protects established peers |
| **Proof-of-work for IDs** | Computational cost for node IDs | Limits Sybil creation rate |
| **Trusted peer lists** | Out-of-band peer exchange | Bypass attacker-controlled discovery |
| **Network monitoring** | Detect anomalous connection patterns | Early warning system |
| **Routing table persistence** | Survive restarts with known-good peers | Prevent fresh-start attacks |

### Mitigation Comparison Matrix

| Mitigation | Eclipse Resistance | Sybil Resistance | Complexity | Performance Impact |
|------------|-------------------|------------------|------------|-------------------|
| Peer scoring | High | High | Medium | Low |
| Diverse bootstrap | High | Medium | Low | None |
| Connection limits | Medium | Medium | Low | None |
| Slot reservation | Medium | Low | Low | None |
| Reputation system | High | High | High | Medium |
| PoW for IDs | Medium | High | Medium | High (creation only) |

## Recommendations for obsidian-ee

Based on this analysis, we recommend the following for obsidian-ee's P2P implementation:

### Short-term (v2)

1. **Maintain hybrid architecture** - Use relay for MLS, P2P only for CRDT updates
2. **Enable GossipSub peer scoring** - Use recommended parameters from Ethereum research
3. **Operate diverse bootstrap nodes** - Minimum 3 nodes across different infrastructure

### Medium-term (v2.x)

1. **Implement connection diversity** - Subnet and ASN limits
2. **Add reputation persistence** - Survive restarts with peer scores
3. **Monitor for eclipse indicators** - Sudden peer churn, delivery failures

### Long-term (v3)

1. **Consider trusted peer exchange** - Out-of-band sharing of known-good peers
2. **Evaluate stake-based identity** - If applicable to use case
3. **Research mixing networks** - For metadata protection

## References

### Academic Papers

1. Heilman, E., Kendler, A., Zohar, A., & Goldberg, S. (2015). **Eclipse Attacks on Bitcoin's Peer-to-Peer Network**. USENIX Security Symposium. [Link](https://www.usenix.org/conference/usenixsecurity15/technical-sessions/presentation/heilman)

2. Marcus, Y., Heilman, E., & Goldberg, S. (2018). **Low-Resource Eclipse Attacks on Ethereum's Peer-to-Peer Network**. IACR Cryptology ePrint Archive. [Link](https://eprint.iacr.org/2018/236)

3. Xu, G., Guo, B., Su, C., et al. (2020). **Am I Eclipsed? A Smart Detector of Eclipse Attacks for Ethereum**. Computers & Security.

4. Douceur, J. R. (2002). **The Sybil Attack**. IPTPS. [Link](https://www.microsoft.com/en-us/research/publication/the-sybil-attack/)

### Protocol Specifications

5. libp2p GossipSub Specification v1.1. [GitHub](https://github.com/libp2p/specs/blob/master/pubsub/gossipsub/gossipsub-v1.1.md)

6. Ethereum Consensus Layer Networking Specification. [GitHub](https://github.com/ethereum/consensus-specs/blob/dev/specs/phase0/p2p-interface.md)

7. Kademlia: A Peer-to-peer Information System Based on the XOR Metric. [Paper](https://pdos.csail.mit.edu/~petar/papers/maymounkov-kademlia-lncs.pdf)

### Implementation Resources

8. rust-libp2p Documentation. [Docs.rs](https://docs.rs/libp2p/latest/libp2p/)

9. go-libp2p-pubsub Peer Scoring. [GitHub](https://github.com/libp2p/go-libp2p-pubsub/blob/master/score.go)

10. Ethereum P2P Network Security Analysis. [Ethereum Research](https://ethresear.ch/)

---

*Last updated: 2024*
*Applies to: obsidian-ee v2.x P2P architecture*
