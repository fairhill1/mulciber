# mulciber-net

Game networking over UDP for a dedicated server and its clients, independent of Mulciber's
graphics. The model is Quake 3's and Source's; see `docs/net-contract.md` in the repository.

- **Connecting**: a client's request is answered with a cookie proving it owns its address before
  the server gives it a slot. Different protocol versions refuse each other.
- **Reliable messages** arrive once each, in order, at any size up to a configured limit: they are
  split into fragments and resent until acknowledged.
- **Unreliable messages** go out once; one larger than a packet arrives whole or not at all.
- **Acks** on every packet give each side its round-trip time and packet loss.
- **Transports**: real UDP sockets, an in-process network, and a conditioner that adds latency,
  jitter, loss and duplication to either.
- Every call takes the caller's `now`, so a test steps a fake clock through minutes in milliseconds.

```rust
use std::time::{Duration, Instant};
use mulciber_net::{Channel, Client, ClientState, Config, Server, ServerConfig, ServerEvent, UdpTransport};

let config = Config::new(0x5348_4950_0001);
let transport = UdpTransport::bind("127.0.0.1:0")?;
let address = transport.local_address()?;
let mut server = Server::new(transport, ServerConfig::new(config, 32));
let mut client = Client::connect(UdpTransport::bind("127.0.0.1:0")?, address, config, b"Ann", Instant::now())?;

while client.state() == ClientState::Connecting {
    let now = Instant::now();
    server.update(now)?;
    client.update(now)?;
    server.flush(now)?;
    client.flush(now)?;
    std::thread::sleep(Duration::from_millis(1));
}
if let Some(ServerEvent::Connected { client: id, .. }) = server.poll_event() {
    server.send(id, Channel::Reliable, b"welcome aboard")?;
}
# Ok::<(), Box<dyn std::error::Error>>(())
```
