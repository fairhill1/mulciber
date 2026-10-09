# Networking contract (`mulciber-net`)

`mulciber-net` connects a dedicated server to its clients over UDP. It carries bytes; what they
mean, how often they go and how the game hides latency (prediction, interpolation, lag
compensation) are the game's. Shiplike is the first consumer.

## Model

The client/server model of Quake 3 and Source, as described in Valve's "Source Multiplayer
Networking" and Yahn Bernier's "Latency Compensating Methods in Client/Server In-game Protocol
Design and Optimization", and the packet layer of Glenn Fiedler's Gaffer On Games articles
("Reliability and Congestion Avoidance", "Packet Fragmentation and Reassembly", "Sending Large
Blocks of Data", "Reliable Ordered Messages"). No code is taken from any of them.

## Guarantees

- **Connecting.** `ConnectRequest` (padded to 1200 bytes) → `Challenge` (a keyed hash of the
  address, the client's nonce and a 10 s window) → `ChallengeResponse` (the cookie and up to
  1024 bytes of game payload) → `Accepted` (a random 64-bit session) or `Denied` (server full,
  wrong protocol). A server holds no state for an address until its cookie comes back, and never
  answers with more bytes than it was sent. Requests and responses repeat every 100 ms.
- **Sessions.** Every packet after connecting carries the session; others are dropped. It stops
  blind spoofing, not an attacker who sees the traffic: packets are not encrypted.
- **Packets.** At most 1200 bytes. Each carries a 16-bit sequence number and acknowledges the
  latest received, with how long it was held, plus the 32 before it; a flush adds bare acks for
  anything older received since the last, so bursts are acknowledged in full. Duplicates and packets more than 1024 behind are
  dropped whole; malformed ones are dropped without effect.
- **Reliable channel.** Messages up to `Config::max_message_size` (1 MiB by default), split into
  1024-byte fragments, at most 1024 in flight. A fragment goes again after 1.5 × the round-trip
  time plus 30 ms until acknowledged. Delivered once, in order. A peer whose message would exceed
  the limit is disconnected as misbehaving. `Config::reliable_bytes_per_flush` caps each flush;
  `Config::max_backlog` caps what waits, after which `send` fails with `Backlogged`.
- **Unreliable channel.** Sent once in the next flush. Up to 255 fragments; a split message is
  delivered only when every fragment arrives within a second, and at most once.
- **Ordering across channels** is not kept: a packet carries reliable fragments first.
- **Liveness.** A connection sends at least once a second; silence for `Config::timeout` (10 s)
  ends it. Goodbyes are sent three times. Messages received before a disconnect stay readable.
- **Stats.** Round-trip time (smoothed; each ack says how long it was held before being sent, in
  100 µs units, and that is subtracted, so the time is the network's alone), packet loss over
  roughly the last 20 packets that carried messages, and bytes a second each way.

## Transports

- `UdpTransport`: a non-blocking socket. Per-packet errors (a full buffer, ICMP unreachable,
  oversized datagrams on Windows) are dropped as packet loss.
- `MemoryTransport`: an in-process network for tests and for running server and client together.
- `Conditioned<T>`: holds, drops and doubles one endpoint's packets both ways, from a seed.

## Evidence

`cargo test -p mulciber-net`: unit tests of sequence wrapping, the wire format, acks, resends,
burst acknowledgement, fragment reassembly, duplicates and the message limit; end-to-end tests of
connecting, both channels across a link with 60–100 ms latency each way, 20% loss and 10%
duplication (300 reliable messages up to 200 KB each way, delivered exactly and in order), full
servers, wrong protocols, goodbyes, timeouts, a client restarting on the same address, 20,000
random and spoofed packets, and real UDP sockets on loopback. A one-off run across 40 seeds at 45%
loss each way, 30% duplication and 200 ms of jitter also delivered everything, in order.

Not yet exercised: two machines over a real network, IPv6, NAT, Steam's relay network.
