//! Servers and clients end to end, over a network in memory (made as bad as needed) and over real
//! UDP sockets.

use std::time::{Duration, Instant};

use mulciber_net::{
    Channel, Client, ClientId, ClientState, Conditioned, Conditions, Config, DisconnectReason,
    MemoryAddress, MemoryNetwork, MemoryTransport, Server, ServerConfig, ServerEvent, Transport,
    UdpTransport,
};

const CONFIG: Config = Config::new(0x5348_4950);
const TICK: Duration = Duration::from_millis(16);
const SERVER: MemoryAddress = MemoryAddress(1);

/// One tick of a server and its clients.
fn tick<S: Transport, C: Transport>(
    now: Instant,
    server: &mut Server<S>,
    clients: &mut [&mut Client<C>],
) {
    server.update(now).unwrap();
    for client in clients.iter_mut() {
        client.update(now).unwrap();
    }
    server.flush(now).unwrap();
    for client in clients.iter_mut() {
        client.flush(now).unwrap();
    }
}

fn events<T: Transport>(server: &mut Server<T>) -> Vec<ServerEvent> {
    std::iter::from_fn(|| server.poll_event()).collect()
}

fn connected<T: Transport>(server: &mut Server<T>) -> ClientId {
    match &events(server)[..] {
        [ServerEvent::Connected { client, .. }] => *client,
        other => panic!("{other:?}"),
    }
}

fn setup(network: &MemoryNetwork, max_clients: usize) -> Server<MemoryTransport> {
    Server::new(
        network.endpoint(SERVER),
        ServerConfig::new(CONFIG, max_clients),
    )
}

#[test]
fn a_client_connects_and_both_sides_talk() {
    let network = MemoryNetwork::new();
    let mut server = setup(&network, 4);
    let mut now = Instant::now();
    let mut client = Client::connect(
        network.endpoint(MemoryAddress(2)),
        SERVER,
        CONFIG,
        b"Ann",
        now,
    )
    .unwrap();
    for _ in 0..3 {
        now += TICK;
        tick(now, &mut server, &mut [&mut client]);
    }
    assert_eq!(client.state(), ClientState::Connected);
    let events = events(&mut server);
    let [
        ServerEvent::Connected {
            client: id,
            payload,
        },
    ] = &events[..]
    else {
        panic!("{events:?}")
    };
    assert_eq!(&**payload, b"Ann");
    assert_eq!(server.address(*id), Some(MemoryAddress(2)));

    client.send(Channel::Unreliable, b"forward").unwrap();
    client.send(Channel::Reliable, b"open the door").unwrap();
    server.send(*id, Channel::Reliable, b"door opened").unwrap();
    now += TICK;
    tick(now, &mut server, &mut [&mut client]);
    now += TICK;
    tick(now, &mut server, &mut [&mut client]);
    // Order holds within a channel, not across them: a packet carries its reliable fragments first.
    assert_eq!(&*server.receive(*id).unwrap().data, b"open the door");
    assert_eq!(&*server.receive(*id).unwrap().data, b"forward");
    assert_eq!(&*client.receive().unwrap().data, b"door opened");
}

/// A lossy, laggy, jittery and duplicating link (the client's), both ends of which see every reliable
/// message once and in order, and every unreliable one at most once and whole.
#[test]
fn messages_survive_a_bad_network() {
    let network = MemoryNetwork::new();
    let bad = Conditions {
        latency: Duration::from_millis(60),
        jitter: Duration::from_millis(40),
        loss: 0.2,
        duplicate: 0.1,
    };
    let mut server = setup(&network, 4);
    let mut now = Instant::now();
    let transport = Conditioned::new(network.endpoint(MemoryAddress(2)), bad, 2);
    let mut client = Client::connect(transport, SERVER, CONFIG, b"", now).unwrap();
    while client.state() == ClientState::Connecting {
        now += TICK;
        tick(now, &mut server, &mut [&mut client]);
    }
    let id = connected(&mut server);

    // Reliable messages of every size, a 200 KB one among them, and a stream of unreliable ones.
    let reliable: Vec<Vec<u8>> = (0..300u32)
        .map(|i| {
            let size = if i == 150 {
                200_000
            } else {
                (i as usize * 37) % 3000
            };
            (0..size)
                .map(|b| (b ^ i as usize).to_le_bytes()[0])
                .collect()
        })
        .collect();
    let mut sent_unreliable = 0u32;
    let (mut to_server, mut to_client, mut unreliable_seen) = (Vec::new(), Vec::new(), Vec::new());
    for step in 0..2000 {
        if step < reliable.len() {
            client.send(Channel::Reliable, &reliable[step]).unwrap();
            server.send(id, Channel::Reliable, &reliable[step]).unwrap();
        }
        if step < 1000 {
            // Every tenth is too big for a packet.
            let size = if step % 10 == 0 { 4000 } else { 40 };
            let mut message = sent_unreliable.to_le_bytes().to_vec();
            message.resize(size, (sent_unreliable % 251) as u8);
            server.send(id, Channel::Unreliable, &message).unwrap();
            sent_unreliable += 1;
        }
        now += TICK;
        tick(now, &mut server, &mut [&mut client]);
        while let Some(message) = server.receive(id) {
            to_server.push(message.data.to_vec());
        }
        while let Some(message) = client.receive() {
            match message.channel {
                Channel::Reliable => to_client.push(message.data.to_vec()),
                Channel::Unreliable => {
                    let n = u32::from_le_bytes(message.data[..4].try_into().unwrap());
                    assert!(message.data[4..].iter().all(|&b| b == (n % 251) as u8));
                    assert_eq!(message.data.len(), if n % 10 == 0 { 4000 } else { 40 });
                    unreliable_seen.push(n);
                }
            }
        }
    }
    assert_eq!(client.state(), ClientState::Connected);
    assert!(
        to_server == reliable,
        "the server got {} of {} reliable messages, or out of order",
        to_server.len(),
        reliable.len()
    );
    assert!(
        to_client == reliable,
        "the client got {} of {} reliable messages, or out of order",
        to_client.len(),
        reliable.len()
    );
    let mut unique = unreliable_seen.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        unreliable_seen.len(),
        "an unreliable message arrived twice"
    );
    // About 80% of the small ones, and fewer of the split ones, which need all of four fragments.
    let small = unique.iter().filter(|n| *n % 10 != 0).count();
    assert!(
        (600..800).contains(&small),
        "{small} of 900 small unreliable messages arrived"
    );

    let stats = client.stats().unwrap();
    // A ping of 120 to 200 ms, from the latency and jitter each way, plus up to a tick for the reply.
    assert!(
        stats.rtt > Duration::from_millis(120) && stats.rtt < Duration::from_millis(260),
        "{stats:?}"
    );
    // A fifth of the client's packets are lost; acks, repeated in 33 packets, all but never are.
    assert!(
        stats.packet_loss > 0.08 && stats.packet_loss < 0.4,
        "{stats:?}"
    );
}

#[test]
fn a_full_server_turns_clients_away() {
    let network = MemoryNetwork::new();
    let mut server = setup(&network, 1);
    let mut now = Instant::now();
    let mut first =
        Client::connect(network.endpoint(MemoryAddress(2)), SERVER, CONFIG, b"", now).unwrap();
    let mut second =
        Client::connect(network.endpoint(MemoryAddress(3)), SERVER, CONFIG, b"", now).unwrap();
    for _ in 0..5 {
        now += TICK;
        tick(now, &mut server, &mut [&mut first, &mut second]);
    }
    assert_eq!(first.state(), ClientState::Connected);
    assert_eq!(
        second.state(),
        ClientState::Disconnected(DisconnectReason::ServerFull)
    );
    assert_eq!(server.clients().count(), 1);
}

#[test]
fn a_different_protocol_is_refused() {
    let network = MemoryNetwork::new();
    let mut server = setup(&network, 4);
    let mut now = Instant::now();
    let other = Config::new(CONFIG.protocol + 1);
    let mut client =
        Client::connect(network.endpoint(MemoryAddress(2)), SERVER, other, b"", now).unwrap();
    for _ in 0..3 {
        now += TICK;
        tick(now, &mut server, &mut [&mut client]);
    }
    assert_eq!(
        client.state(),
        ClientState::Disconnected(DisconnectReason::WrongProtocol)
    );
    assert_eq!(events(&mut server), []);
}

#[test]
fn hanging_up_reaches_the_other_side_and_silence_times_out() {
    let network = MemoryNetwork::new();
    let mut server = setup(&network, 4);
    let mut now = Instant::now();
    let mut leaver =
        Client::connect(network.endpoint(MemoryAddress(2)), SERVER, CONFIG, b"", now).unwrap();
    let mut vanisher =
        Client::connect(network.endpoint(MemoryAddress(3)), SERVER, CONFIG, b"", now).unwrap();
    for _ in 0..3 {
        now += TICK;
        tick(now, &mut server, &mut [&mut leaver, &mut vanisher]);
    }
    let ids: Vec<ClientId> = events(&mut server)
        .into_iter()
        .map(|event| match event {
            ServerEvent::Connected { client, .. } => client,
            other @ ServerEvent::Disconnected { .. } => panic!("{other:?}"),
        })
        .collect();

    // A last message, then goodbye: the server still reads the message.
    leaver.send(Channel::Reliable, b"farewell").unwrap();
    leaver.flush(now).unwrap();
    leaver.disconnect(now);
    assert_eq!(
        leaver.state(),
        ClientState::Disconnected(DisconnectReason::ClosedLocally)
    );
    now += TICK;
    tick(now, &mut server, &mut [&mut vanisher]);
    let leaver_id = server.clients().find(|id| !ids[1..].contains(id));
    assert_eq!(leaver_id, None);
    assert_eq!(&*server.receive(ids[0]).unwrap().data, b"farewell");
    assert_eq!(
        events(&mut server),
        [ServerEvent::Disconnected {
            client: ids[0],
            reason: DisconnectReason::ClosedByPeer
        }]
    );

    // The other one's machine goes away: after the timeout, the server lets it go.
    drop(vanisher);
    let mut gone = Vec::new();
    while gone.is_empty() {
        now += TICK;
        server.update(now).unwrap();
        server.flush(now).unwrap();
        gone = events(&mut server);
    }
    assert_eq!(
        gone,
        [ServerEvent::Disconnected {
            client: ids[1],
            reason: DisconnectReason::TimedOut
        }]
    );
}

#[test]
fn a_client_learns_the_server_is_gone() {
    let network = MemoryNetwork::new();
    let mut server = setup(&network, 4);
    let mut now = Instant::now();
    let mut client =
        Client::connect(network.endpoint(MemoryAddress(2)), SERVER, CONFIG, b"", now).unwrap();
    for _ in 0..3 {
        now += TICK;
        tick(now, &mut server, &mut [&mut client]);
    }
    let id = connected(&mut server);
    server.send(id, Channel::Reliable, b"last orders").unwrap();
    server.flush(now).unwrap();
    server.disconnect(id, now);
    client.update(now).unwrap();
    assert_eq!(
        client.state(),
        ClientState::Disconnected(DisconnectReason::ClosedByPeer)
    );
    // What came before the goodbye is still there to read.
    assert_eq!(&*client.receive().unwrap().data, b"last orders");
    assert_eq!(
        client.send(Channel::Reliable, b"?"),
        Err(mulciber_net::SendError::NotConnected)
    );

    let mut lonely = Client::connect(
        network.endpoint(MemoryAddress(3)),
        MemoryAddress(9),
        CONFIG,
        b"",
        now,
    )
    .unwrap();
    let start = now;
    while lonely.state() == ClientState::Connecting {
        now += TICK;
        lonely.update(now).unwrap();
        lonely.flush(now).unwrap();
    }
    assert_eq!(
        lonely.state(),
        ClientState::Disconnected(DisconnectReason::TimedOut)
    );
    assert!(now - start > CONFIG.timeout);
}

#[test]
fn a_client_starting_over_from_the_same_address_replaces_its_old_self() {
    let network = MemoryNetwork::new();
    let mut server = setup(&network, 4);
    let mut now = Instant::now();
    let mut client =
        Client::connect(network.endpoint(MemoryAddress(2)), SERVER, CONFIG, b"", now).unwrap();
    for _ in 0..3 {
        now += TICK;
        tick(now, &mut server, &mut [&mut client]);
    }
    let old = connected(&mut server);
    // The game crashes and restarts on the same port without saying goodbye.
    drop(client);
    let mut client =
        Client::connect(network.endpoint(MemoryAddress(2)), SERVER, CONFIG, b"", now).unwrap();
    for _ in 0..3 {
        now += TICK;
        tick(now, &mut server, &mut [&mut client]);
    }
    assert_eq!(client.state(), ClientState::Connected);
    let events = events(&mut server);
    assert_eq!(
        events[0],
        ServerEvent::Disconnected {
            client: old,
            reason: DisconnectReason::ClosedByPeer
        }
    );
    assert!(matches!(events[1], ServerEvent::Connected { .. }));
    assert_eq!(server.clients().count(), 1);
}

#[test]
fn garbage_and_forged_packets_change_nothing() {
    let network = MemoryNetwork::new();
    let mut server = setup(&network, 4);
    let mut now = Instant::now();
    let mut client =
        Client::connect(network.endpoint(MemoryAddress(2)), SERVER, CONFIG, b"", now).unwrap();
    for _ in 0..3 {
        now += TICK;
        tick(now, &mut server, &mut [&mut client]);
    }
    let id = connected(&mut server);
    let mut attacker = network.endpoint(MemoryAddress(3));
    // Random bytes of every kind and length, including from a connected client's address.
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for n in 0..20_000usize {
        let mut packet = vec![0u8; n % 1300];
        for byte in &mut packet {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *byte = seed.to_le_bytes()[0];
        }
        if let Some(kind) = packet.first_mut() {
            *kind = (n % 10).to_le_bytes()[0];
        }
        attacker.send(now, SERVER, &packet).unwrap();
        if n % 2 == 0 {
            // Spoofed: as if from the client.
            let mut spoof = network.endpoint(MemoryAddress(2));
            spoof.send(now, SERVER, &packet).unwrap();
        }
        if n % 100 == 0 {
            server.update(now).unwrap();
        }
    }
    // The spoofing endpoints took the client's address; give it back.
    let mut client =
        Client::connect(network.endpoint(MemoryAddress(4)), SERVER, CONFIG, b"", now).unwrap();
    for _ in 0..3 {
        now += TICK;
        tick(now, &mut server, &mut [&mut client]);
    }
    assert_eq!(client.state(), ClientState::Connected);
    let events = events(&mut server);
    assert!(
        matches!(&events[..], [ServerEvent::Connected { .. }]),
        "{events:?}"
    );
    assert!(server.clients().any(|c| c == id));
    assert!(server.receive(id).is_none());
}

#[test]
fn over_real_udp_sockets() {
    let transport = UdpTransport::bind("127.0.0.1:0").unwrap();
    let address = transport.local_address().unwrap();
    let mut server = Server::new(transport, ServerConfig::new(CONFIG, 4));
    let start = Instant::now();
    let mut client = Client::connect(
        UdpTransport::bind("127.0.0.1:0").unwrap(),
        address,
        CONFIG,
        b"udp",
        start,
    )
    .unwrap();
    let mut id = None;
    let mut reply = None;
    let mut sent = false;
    while reply.is_none() && start.elapsed() < Duration::from_secs(5) {
        let now = Instant::now();
        tick(now, &mut server, &mut [&mut client]);
        if let Some(ServerEvent::Connected { client, .. }) = server.poll_event() {
            id = Some(client);
        }
        if let Some(id) = id
            && let Some(message) = server.receive(id)
        {
            server
                .send(id, Channel::Reliable, &[&*message.data, b" back"].concat())
                .unwrap();
        }
        if client.state() == ClientState::Connected && !sent {
            client.send(Channel::Reliable, &[7; 3000]).unwrap();
            sent = true;
        }
        reply = client.receive();
        std::thread::sleep(Duration::from_millis(2));
    }
    let reply = reply.expect("a reply within five seconds");
    assert_eq!(reply.data.len(), 3005);
    assert!(reply.data.ends_with(b" back"));
}
