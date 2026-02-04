# IP Address Leakage in P2P Systems

## Overview

IP address exposure represents one of the most significant privacy and security vulnerabilities in peer-to-peer collaborative editing systems. When participants in a P2P network expose their real IP addresses, they become vulnerable to:

- **Identity Correlation**: Linking pseudonymous document collaborators to real-world identities
- **Targeted Attacks**: Enabling DDoS attacks, port scanning, or exploitation attempts against specific users
- **Geolocation Tracking**: Determining physical locations through IP geolocation databases
- **Legal/Regulatory Exposure**: Creating audit trails that may be compelled through legal process
- **Surveillance**: Enabling network-level monitoring by ISPs, governments, or malicious actors

In collaborative editing contexts, IP leakage is particularly concerning because it creates a persistent association between a user's network identity and their document activity, enabling long-term tracking of editing patterns and social graphs.

---

## Technical Background

### How P2P Connections Expose IP Addresses

Peer-to-peer systems fundamentally require participants to establish direct network connections. Unlike client-server architectures where the server acts as an intermediary, P2P protocols necessitate that peers discover and connect to each other directly, inherently exposing network-layer information.

```mermaid
sequenceDiagram
    participant A as Peer A<br/>192.168.1.100
    participant S as Signaling Server
    participant B as Peer B<br/>10.0.0.50

    A->>S: Register presence + public IP
    B->>S: Register presence + public IP
    A->>S: Request peer list
    S->>A: Peer B at 203.0.113.42:8080
    A->>B: Direct connection (exposes A's IP)
    B->>A: Direct connection (exposes B's IP)
    Note over A,B: Both peers now know each other's<br/>public IP addresses
```

### WebRTC ICE Candidate Gathering

WebRTC-based P2P systems (like y-webrtc) use Interactive Connectivity Establishment (ICE) to traverse NATs and firewalls. This process systematically discovers and exposes all network interfaces:

```mermaid
flowchart TB
    subgraph "ICE Candidate Gathering"
        A[Start Gathering] --> B[Host Candidates]
        B --> C[STUN Reflexive Candidates]
        C --> D[TURN Relay Candidates]
    end

    subgraph "Host Candidates"
        B --> B1[Local IPv4: 192.168.1.100]
        B --> B2[Local IPv6: fe80::1]
        B --> B3[VPN Interface: 10.8.0.5]
    end

    subgraph "STUN Server Response"
        C --> C1[Public IPv4: 203.0.113.42]
        C --> C2[Public IPv6: 2001:db8::1]
        C --> C3[NAT Type Detection]
    end

    subgraph "Information Exposed"
        E[All candidates sent to peers]
        B1 --> E
        B2 --> E
        B3 --> E
        C1 --> E
        C2 --> E
    end

    style E fill:#ff6b6b,stroke:#333,stroke-width:2px
```

#### ICE Candidate Types

| Candidate Type | Source | Information Leaked |
|---------------|--------|-------------------|
| `host` | Local interfaces | Private IPs, all network interfaces, VPN presence |
| `srflx` (Server Reflexive) | STUN server | Public IP, NAT type, port allocation pattern |
| `prflx` (Peer Reflexive) | Peer connection | Public IP as seen by peer |
| `relay` | TURN server | Only TURN server IP (privacy-preserving) |

### Session Description Protocol (SDP) Exposure

WebRTC connections exchange SDP offers/answers containing sensitive network information:

```
v=0
o=- 4611731400430051336 2 IN IP4 127.0.0.1
s=-
t=0 0
a=group:BUNDLE 0
a=msid-semantic: WMS
m=application 9 UDP/DTLS/SCTP webrtc-datachannel
c=IN IP4 0.0.0.0
a=candidate:1 1 UDP 2122252543 192.168.1.100 49170 typ host
a=candidate:2 1 UDP 1686052863 203.0.113.42 49170 typ srflx raddr 192.168.1.100 rport 49170
a=candidate:3 1 UDP 41885439 198.51.100.1 52739 typ relay raddr 203.0.113.42 rport 49170
a=ice-ufrag:EsAw
a=ice-pwd:P2uYro0UCOQ4zxjKXaWCBui1
a=fingerprint:sha-256 D2:B9:31:...
```

The SDP above reveals:
- Private IP: `192.168.1.100`
- Public IP: `203.0.113.42`
- NAT mapping: private:49170 → public:49170
- TURN relay: `198.51.100.1`

---

## Risk by Library

### y-webrtc: HIGH Risk

y-webrtc implements WebRTC data channels for Yjs synchronization, inheriting all WebRTC privacy concerns.

```mermaid
flowchart LR
    subgraph "y-webrtc Connection Flow"
        A[Peer A] -->|1. Join Room| S[Signaling Server]
        B[Peer B] -->|1. Join Room| S
        S -->|2. Exchange SDPs| A
        S -->|2. Exchange SDPs| B
        A <-->|3. ICE Candidates| STUN[STUN Server]
        B <-->|3. ICE Candidates| STUN
        A <-.->|4. Direct P2P| B
    end

    subgraph "Information Exposed"
        E1[Public IP via STUN]
        E2[Private IPs in SDP]
        E3[All network interfaces]
        E4[NAT traversal details]
    end

    STUN --> E1
    A --> E2
    A --> E3
    A --> E4

    style E1 fill:#ff6b6b
    style E2 fill:#ff6b6b
    style E3 fill:#ff6b6b
    style E4 fill:#ff6b6b
```

#### Specific Vulnerabilities

1. **Default STUN Servers**: y-webrtc defaults to Google's public STUN servers (`stun:stun.l.google.com:19302`), which:
   - Log connection metadata
   - Reveal user's public IP to Google
   - Create correlation opportunities

2. **Signaling Server Exposure**: The signaling server sees:
   - All peer IP addresses
   - Room membership (document access patterns)
   - Connection timing metadata

3. **Peer-to-Peer SDP Exchange**: Every peer receives full ICE candidate lists from every other peer, meaning:
   - Any malicious peer can harvest all participants' IPs
   - No authentication required to receive SDP information
   - Historical SDPs may be logged by signaling infrastructure

4. **No Built-in Relay Mode**: y-webrtc has no configuration to force TURN-only connections, making IP exposure unavoidable without significant modification.

#### Code-Level Exposure Points

```typescript
// y-webrtc creates connections that expose IPs
const provider = new WebrtcProvider('document-room', ydoc, {
  signaling: ['wss://signaling.example.com'],
  // No option to disable host candidates
  // No option to force TURN-only
})

// ICE candidates automatically shared with all peers
peerConnection.onicecandidate = (event) => {
  if (event.candidate) {
    // This candidate contains IP information
    signalingChannel.send({
      type: 'candidate',
      candidate: event.candidate // Includes host/srflx candidates
    })
  }
}
```

### y-libp2p: MEDIUM Risk

libp2p-based implementations have different exposure characteristics due to the protocol's architecture.

```mermaid
flowchart TB
    subgraph "libp2p Identity Layer"
        PK[Private Key] --> PID[Peer ID<br/>QmYwAPJzv5CZsnA...]
        PID --> ADDR[Multiaddress<br/>/ip4/203.0.113.42/tcp/4001/p2p/QmYwAP...]
    end

    subgraph "Discovery Mechanisms"
        DHT[Kademlia DHT]
        MDNS[mDNS Local Discovery]
        BOOT[Bootstrap Nodes]
    end

    subgraph "Information Flow"
        ADDR --> DHT
        ADDR --> MDNS
        ADDR --> BOOT
        DHT --> |Propagates to| NET[Entire Network]
    end

    subgraph "Risk Factors"
        R1[Peer ID is persistent identity]
        R2[Multiaddrs contain IPs]
        R3[DHT queries reveal interests]
        R4[Connection patterns visible]
    end

    style R1 fill:#ffa500
    style R2 fill:#ffa500
    style R3 fill:#ffa500
    style R4 fill:#ffa500
```

#### Specific Vulnerabilities

1. **Persistent Peer IDs**: Unlike ephemeral WebRTC connections, libp2p Peer IDs are typically long-lived, creating a stable identifier that can be correlated across sessions.

2. **Multiaddress Advertisement**: Peers announce their multiaddresses to the network:
   ```
   /ip4/203.0.113.42/tcp/4001/p2p/QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG
   /ip6/2001:db8::1/tcp/4001/p2p/QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG
   ```

3. **DHT Participation**: Joining the Kademlia DHT for peer discovery means:
   - Publishing your addresses to routing tables
   - Queries reveal which content/peers you're seeking
   - Sybil attacks can target specific peer ranges

4. **mDNS Local Discovery**: Local network discovery broadcasts peer presence to all devices on the LAN.

#### Mitigating Factors (vs WebRTC)

| Factor | y-webrtc | y-libp2p |
|--------|----------|----------|
| Relay Support | Limited | Native circuit relay |
| Transport Flexibility | WebRTC only | TCP, QUIC, WebSocket, Tor |
| Identity Management | Ephemeral per-connection | Configurable persistence |
| NAT Traversal | Requires STUN/TURN | Multiple strategies |

---

## Attack Scenarios

### Scenario 1: Malicious Peer IP Harvesting

```mermaid
sequenceDiagram
    participant V1 as Victim 1
    participant V2 as Victim 2
    participant V3 as Victim 3
    participant M as Malicious Peer
    participant S as Signaling Server
    participant DB as Attacker's Database

    Note over M: Attacker joins document room

    M->>S: Join "confidential-document-123"
    V1->>S: Join same room
    V2->>S: Join same room
    V3->>S: Join same room

    S->>M: Peer list + SDPs

    Note over M: Extract IPs from ICE candidates

    M->>DB: Store: V1 = 203.0.113.10
    M->>DB: Store: V2 = 198.51.100.25
    M->>DB: Store: V3 = 192.0.2.100

    Note over M: Correlate with document metadata

    M->>DB: Document "confidential-document-123"<br/>accessed by IPs: [...]
    M->>DB: Geolocation lookup
    M->>DB: Reverse DNS lookup
    M->>DB: Historical IP correlation

    Note over DB: Attacker now has:<br/>- Real IPs of all collaborators<br/>- Document access patterns<br/>- Physical locations<br/>- Potential real identities
```

### Scenario 2: Passive Network Observer

```mermaid
flowchart TB
    subgraph "Collaborative Session"
        P1[Peer 1] <--> P2[Peer 2]
        P2 <--> P3[Peer 3]
        P1 <--> P3
    end

    subgraph "Network Observation Points"
        ISP[ISP Monitoring]
        CORP[Corporate Firewall]
        GOV[Government Surveillance]
    end

    subgraph "Observable Metadata"
        M1[Connection timing]
        M2[Data volume patterns]
        M3[Peer IP addresses]
        M4[Session duration]
        M5[STUN server queries]
    end

    P1 --> ISP
    P2 --> CORP
    P3 --> GOV

    ISP --> M1
    ISP --> M2
    ISP --> M3
    CORP --> M4
    GOV --> M5

    subgraph "Intelligence Derived"
        I1[Social graph construction]
        I2[Activity pattern analysis]
        I3[Identity correlation]
    end

    M1 --> I1
    M3 --> I2
    M5 --> I3

    style ISP fill:#ff6b6b
    style CORP fill:#ff6b6b
    style GOV fill:#ff6b6b
```

### Scenario 3: Targeted Attack Chain

```mermaid
flowchart LR
    subgraph "Phase 1: Reconnaissance"
        A1[Join target document] --> A2[Harvest peer IPs]
        A2 --> A3[Geolocation lookup]
        A3 --> A4[Identify high-value targets]
    end

    subgraph "Phase 2: Enumeration"
        A4 --> B1[Port scan victim IPs]
        B1 --> B2[Service fingerprinting]
        B2 --> B3[Vulnerability assessment]
    end

    subgraph "Phase 3: Exploitation"
        B3 --> C1[Exploit vulnerable services]
        C1 --> C2[Lateral movement]
        C2 --> C3[Data exfiltration]
    end

    subgraph "Alternative: DoS"
        A4 --> D1[DDoS victim IPs]
        D1 --> D2[Disrupt collaboration]
    end

    style A1 fill:#ffa500
    style C3 fill:#ff6b6b
    style D2 fill:#ff6b6b
```

---

## Impact Assessment

### Privacy Impacts

| Impact Category | Severity | Description |
|----------------|----------|-------------|
| **Deanonymization** | Critical | Pseudonymous collaborators can be linked to real identities through IP geolocation, ISP records, or correlation with other services |
| **Location Tracking** | High | IP addresses reveal approximate physical location (city-level typically, sometimes more precise) |
| **Activity Correlation** | High | Cross-session tracking enables building comprehensive user profiles |
| **Social Graph Exposure** | Medium | Document co-authorship reveals professional and personal relationships |

### Security Impacts

| Impact Category | Severity | Description |
|----------------|----------|-------------|
| **Targeted Attacks** | Critical | Exposed IPs enable direct attacks against specific collaborators |
| **DDoS Vulnerability** | High | Any peer can be targeted for denial of service |
| **Network Reconnaissance** | Medium | IP addresses provide entry point for further enumeration |
| **Lateral Movement** | Medium | Compromising one peer may reveal paths to others via IP knowledge |

### Regulatory/Legal Impacts

| Impact Category | Severity | Description |
|----------------|----------|-------------|
| **GDPR Compliance** | High | IP addresses are personal data under GDPR; exposure may violate data protection requirements |
| **Subpoena Risk** | Medium | IP logs at signaling/STUN servers can be legally compelled |
| **Jurisdictional Issues** | Medium | Cross-border IP exposure may implicate multiple legal regimes |

---

## Mitigations

### 1. TURN Relay-Only Mode

Force all connections through TURN relays, hiding peer IP addresses from each other.

```mermaid
flowchart TB
    subgraph "TURN-Only Architecture"
        P1[Peer 1<br/>IP Hidden] <-->|Encrypted| TURN[TURN Relay<br/>198.51.100.1]
        TURN <-->|Encrypted| P2[Peer 2<br/>IP Hidden]
    end

    subgraph "What Each Party Sees"
        P1V[Peer 1 sees:<br/>TURN server IP only]
        P2V[Peer 2 sees:<br/>TURN server IP only]
        TV[TURN sees:<br/>Both peer IPs]
    end

    P1 --> P1V
    P2 --> P2V
    TURN --> TV

    style P1V fill:#90EE90
    style P2V fill:#90EE90
    style TV fill:#ffa500
```

**Implementation:**
```typescript
const peerConnection = new RTCPeerConnection({
  iceServers: [{
    urls: 'turn:turn.example.com:443',
    username: 'user',
    credential: 'pass'
  }],
  iceTransportPolicy: 'relay' // Force TURN-only
})
```

**Tradeoffs:**
- (+) Peers cannot see each other's IPs
- (+) Works reliably through restrictive NATs
- (-) TURN server sees all peer IPs
- (-) Increased latency (all traffic relayed)
- (-) Higher infrastructure costs

### 2. Tor/I2P Transport Layer

Route all P2P traffic through anonymity networks.

```mermaid
flowchart LR
    subgraph "Tor Transport"
        P1[Peer 1] --> T1[Tor Entry]
        T1 --> T2[Tor Middle]
        T2 --> T3[Tor Exit/Onion]
        T3 --> P2[Peer 2 / .onion]
    end

    subgraph "Properties"
        PR1[3+ hop routing]
        PR2[Onion encryption]
        PR3[IP hidden from all peers]
        PR4[High latency ~200-500ms]
    end

    style T1 fill:#90EE90
    style T2 fill:#90EE90
    style T3 fill:#90EE90
```

**libp2p Tor Configuration:**
```rust
// Using arti (Tor implementation in Rust)
let transport = TorTransport::new()
    .with_onion_service(onion_config)
    .boxed();

let swarm = SwarmBuilder::new()
    .with_transport(transport)
    .build();
```

**Tradeoffs:**
- (+) Strong anonymity (no single point sees full path)
- (+) Resistant to traffic analysis
- (-) High latency (unsuitable for real-time collaboration)
- (-) Complex deployment
- (-) Exit node risks for clearnet destinations

### 3. VPN-Based Protection

Require VPN usage to mask real IP addresses.

```mermaid
flowchart TB
    subgraph "VPN Architecture"
        P1[Peer 1<br/>Real: 192.168.1.100] --> VPN1[VPN Server A<br/>Exit: 203.0.113.1]
        P2[Peer 2<br/>Real: 10.0.0.50] --> VPN2[VPN Server B<br/>Exit: 198.51.100.1]
        VPN1 <--> S[Signaling/Relay]
        VPN2 <--> S
    end

    subgraph "Visibility"
        V1[Peers see VPN IPs]
        V2[VPN provider sees real IPs]
        V3[ISP sees VPN connection only]
    end

    style V1 fill:#90EE90
    style V2 fill:#ffa500
    style V3 fill:#90EE90
```

**Tradeoffs:**
- (+) Easy to deploy (user responsibility)
- (+) Works with existing P2P protocols
- (-) VPN provider becomes trusted party
- (-) VPN exit IPs can still be correlated
- (-) Not enforced at protocol level

### 4. Relay-Only Architecture (Recommended)

Eliminate P2P connections entirely by routing all traffic through encrypted relays.

```mermaid
flowchart TB
    subgraph "Relay-Only Design"
        P1[Peer 1] -->|E2E Encrypted| R[Relay Server]
        P2[Peer 2] -->|E2E Encrypted| R
        P3[Peer 3] -->|E2E Encrypted| R
    end

    subgraph "Security Properties"
        S1[Relay sees: encrypted blobs + IPs]
        S2[Peers see: only relay IP]
        S3[Content: end-to-end encrypted]
    end

    R --> S1
    P1 --> S2
    P2 --> S2
    P3 --> S2

    subgraph "Additional Measures"
        A1[Multiple relay hops]
        A2[Tor .onion relay access]
        A3[Mixnet integration]
    end

    style S2 fill:#90EE90
    style S3 fill:#90EE90
    style S1 fill:#ffa500
```

**This is the architecture adopted by obsidian-ee:**
- All synchronization traffic routes through the relay server
- MLS provides end-to-end encryption (relay cannot read content)
- Peers never establish direct connections
- Relay sees client IPs but not document contents

**Additional hardening:**
```rust
// Relay can be accessed via Tor
let relay_addr = "ws://relay.onion:80";

// Or through authenticated TURN
let turn_config = TurnConfig {
    server: "turn:turn.example.com:443",
    transport_policy: TransportPolicy::RelayOnly,
};
```

### 5. Signaling Server Privacy

Minimize metadata exposure at the signaling layer.

| Technique | Description | Effectiveness |
|-----------|-------------|---------------|
| **Ephemeral Room IDs** | Random, single-use room identifiers | Prevents room correlation |
| **Authenticated Access** | Require tokens to join rooms | Prevents open harvesting |
| **Rate Limiting** | Limit peer list requests | Slows bulk harvesting |
| **SDP Filtering** | Remove host candidates before relay | Reduces IP exposure |
| **No Logging** | Don't persist connection metadata | Limits legal exposure |

---

## Comparison Matrix

| Solution | IP Hidden from Peers | IP Hidden from Relay | Latency Impact | Deployment Complexity |
|----------|---------------------|---------------------|----------------|----------------------|
| Direct P2P | No | N/A | Lowest | Low |
| TURN-Only | Yes | No | Medium | Medium |
| VPN | Partially | Partially | Low-Medium | Low (user-managed) |
| Tor/I2P | Yes | Yes | High | High |
| Relay-Only + E2E | Yes | No | Medium | Medium |
| Relay + Tor | Yes | Yes | High | High |

---

## References

### Standards and Specifications

1. **RFC 8445** - Interactive Connectivity Establishment (ICE)
   - https://datatracker.ietf.org/doc/html/rfc8445

2. **RFC 8656** - Traversal Using Relays around NAT (TURN)
   - https://datatracker.ietf.org/doc/html/rfc8656

3. **RFC 7064** - URI Scheme for STUN
   - https://datatracker.ietf.org/doc/html/rfc7064

4. **WebRTC Security Architecture** - W3C
   - https://www.w3.org/TR/webrtc-security/

### Research Papers

5. **"WebRTC IP Address Leakage"** - Security analysis of WebRTC IP handling

6. **"Deanonymizing BitTorrent Users"** - Techniques applicable to P2P CRDT systems

7. **"Kademlia DHT Security Analysis"** - Relevant to libp2p-based systems

### Library Documentation

8. **y-webrtc** - https://github.com/yjs/y-webrtc

9. **libp2p Specifications** - https://github.com/libp2p/specs

10. **rust-libp2p** - https://github.com/libp2p/rust-libp2p

### Privacy Tools

11. **Tor Project** - https://www.torproject.org/

12. **I2P** - https://geti2p.net/

13. **Arti (Tor in Rust)** - https://gitlab.torproject.org/tpo/core/arti
