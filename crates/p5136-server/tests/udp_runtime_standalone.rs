use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::{Duration, Instant},
};

use p5136_core::{
    datagram::{DEFAULT_MAX_DATAGRAM_PAYLOAD, encode_datagram},
    udp_protocol::{
        PqUdpEchoBody, PqUdpTimeSyncBody, PrUdpEchoBody, PrUdpTimeSyncBody, RoutedUdpPacket,
        UdpLogicalBody, encode_routed_udp_packet,
    },
};
use p5136_server::{
    DisconnectOutcome, IdentityBinding, IdentityRegistry, ServerClock, SessionId,
    UdpDispatchAction, UdpDispatchRequest, UdpEndpointBindStatus, UdpEndpointStateError,
    UdpIngress, UdpIngressBody, UdpRuntime, UdpRuntimeConfig, UdpServiceError, UdpTransport,
    WorldHandle, decode_udp_ingress,
};
use tokio::{net::UdpSocket, time::timeout};

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const TEST_TIMEOUT: Duration = Duration::from_secs(2);

#[tokio::test]
async fn real_game_socket_echo_and_time_sync_are_exact_and_monotonic() {
    let (mut runtime, game_client, _) = fixture().await;
    let (_, identity) = active_identity("EchoRider").await;
    let game_endpoint = runtime.endpoints().game;
    runtime
        .service()
        .advance_identity(identity.clone())
        .await
        .unwrap();

    let echo = PqUdpEchoBody {
        value_1: i32::MIN + 5136,
        value_2: -123_456_789,
    };
    send_request(
        &game_client,
        game_endpoint,
        identity.user_no.get(),
        0x1111_2222,
        UdpLogicalBody::PqUdpEcho(echo),
        1,
    )
    .await;
    let ingress = next_ingress(&mut runtime).await;
    let outcome = runtime
        .service()
        .dispatch(request(ingress, &identity, Vec::new()))
        .await
        .unwrap();
    assert_eq!(outcome.action, UdpDispatchAction::EchoReply);
    assert_eq!(outcome.binding_status, UdpEndpointBindStatus::Bound);
    assert_eq!(outcome.sent_datagrams, 1);

    let reply = receive_ingress(&game_client, UdpTransport::Game).await;
    assert_eq!(reply.source, game_endpoint);
    assert_eq!(reply.account_id, identity.user_no.get());
    assert_eq!(reply.route_hash, 0x1111_2222);
    assert_eq!(reply.body, UdpIngressBody::PrUdpEcho(echo.reply()));

    let client_tick = i32::from_le_bytes([0x21, 0x43, 0x65, 0x87]);
    send_request(
        &game_client,
        game_endpoint,
        identity.user_no.get(),
        0x3333_4444,
        UdpLogicalBody::PqUdpTimeSync(PqUdpTimeSyncBody { client_tick }),
        2,
    )
    .await;
    let ingress = next_ingress(&mut runtime).await;
    let outcome = runtime
        .service()
        .dispatch(request(ingress, &identity, Vec::new()))
        .await
        .unwrap();
    assert_eq!(outcome.action, UdpDispatchAction::TimeSyncReply);
    assert_eq!(outcome.binding_status, UdpEndpointBindStatus::Refreshed);
    let first_tick = match receive_ingress(&game_client, UdpTransport::Game).await.body {
        UdpIngressBody::PrUdpTimeSync(reply) => {
            assert_eq!(reply.client_tick, client_tick);
            reply.server_tick
        }
        body => panic!("unexpected time-sync response: {body:?}"),
    };

    tokio::time::sleep(Duration::from_millis(3)).await;
    send_request(
        &game_client,
        game_endpoint,
        identity.user_no.get(),
        0x3333_4444,
        UdpLogicalBody::PqUdpTimeSync(PqUdpTimeSyncBody { client_tick }),
        3,
    )
    .await;
    let ingress = next_ingress(&mut runtime).await;
    runtime
        .service()
        .dispatch(request(ingress, &identity, Vec::new()))
        .await
        .unwrap();
    let second_tick = match receive_ingress(&game_client, UdpTransport::Game).await.body {
        UdpIngressBody::PrUdpTimeSync(reply) => reply.server_tick,
        body => panic!("unexpected time-sync response: {body:?}"),
    };
    assert!(second_tick >= first_tick);
    runtime.shutdown().await;
}

#[tokio::test]
async fn time_sync_uses_the_injected_shared_server_clock() {
    let game_server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let p2p_server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let epoch = Instant::now()
        .checked_sub(Duration::from_millis(5_136))
        .unwrap();
    let clock = ServerClock::from_epoch(epoch);
    let mut runtime = UdpRuntime::spawn_with_clock(
        game_server,
        p2p_server,
        UdpRuntimeConfig::default(),
        clock.clone(),
    )
    .unwrap();
    let (_, identity) = active_identity("SharedClock").await;
    runtime
        .service()
        .advance_identity(identity.clone())
        .await
        .unwrap();

    send_request(
        &client,
        runtime.endpoints().game,
        identity.user_no.get(),
        1,
        UdpLogicalBody::PqUdpTimeSync(PqUdpTimeSyncBody { client_tick: 7 }),
        8,
    )
    .await;
    let ingress = next_ingress(&mut runtime).await;
    let before = clock.tick();
    runtime
        .service()
        .dispatch(request(ingress, &identity, Vec::new()))
        .await
        .unwrap();
    let after = clock.tick();
    let reply = receive_ingress(&client, UdpTransport::Game).await;
    let reply = match reply.body {
        UdpIngressBody::PrUdpTimeSync(reply) => reply,
        body => panic!("unexpected time-sync body: {body:?}"),
    };
    assert!(
        (before..=after).contains(&reply.server_tick),
        "UDP tick must come from the injected process-wide clock"
    );
    assert!(reply.server_tick >= 5_136);
    runtime.shutdown().await;
}

#[tokio::test]
async fn two_clients_relay_exact_game_slot_to_latest_game_endpoint() {
    let (mut runtime, alice_socket, bob_socket) = fixture().await;
    let sessions = session_ids(2).await;
    let mut identities = IdentityRegistry::new();
    let alice = identities.claim(sessions[0], LOOPBACK, "Alice").unwrap();
    let bob = identities.claim(sessions[1], LOOPBACK, "Bob").unwrap();
    let game_endpoint = runtime.endpoints().game;

    bind_with_echo(&mut runtime, &alice_socket, &alice, 0xAAAA_0001).await;
    // A zero target route exercises the required inbound-route fallback.
    bind_with_echo(&mut runtime, &bob_socket, &bob, 0).await;

    let game_body = (0_u8..=127).collect::<Vec<_>>();
    send_request(
        &alice_socket,
        game_endpoint,
        alice.user_no.get(),
        0xBBBB_0002,
        UdpLogicalBody::GameSlotPacket(&game_body),
        0x5136_0001,
    )
    .await;
    let ingress = next_ingress(&mut runtime).await;
    let outcome = runtime
        .service()
        .dispatch(request(
            ingress,
            &alice,
            vec![alice.clone(), bob.clone(), bob.clone()],
        ))
        .await
        .unwrap();
    assert_eq!(outcome.action, UdpDispatchAction::GameSlotRelay);
    assert_eq!(
        outcome.sent_datagrams, 1,
        "self and duplicate targets are skipped"
    );
    assert_eq!(outcome.failed_sends, 0);
    assert_eq!(outcome.unavailable_targets, 0);

    let relayed = receive_ingress(&bob_socket, UdpTransport::Game).await;
    assert_eq!(relayed.source, game_endpoint);
    assert_eq!(relayed.account_id, bob.user_no.get());
    assert_eq!(relayed.route_hash, 0xBBBB_0002);
    assert_eq!(relayed.body, UdpIngressBody::GameSlotPacket(game_body));
    assert_no_datagram(&alice_socket).await;
    runtime.shutdown().await;
}

#[tokio::test]
async fn p2p_ingress_replies_from_p2p_socket_but_game_slot_targets_game_route() {
    let (mut runtime, sender_socket, target_socket) = fixture().await;
    let sessions = session_ids(2).await;
    let mut identities = IdentityRegistry::new();
    let sender = identities
        .claim(sessions[0], LOOPBACK, "P2pSender")
        .unwrap();
    let target = identities
        .claim(sessions[1], LOOPBACK, "GameTarget")
        .unwrap();

    bind_with_echo(&mut runtime, &target_socket, &target, 0xCAFE_BABE).await;
    runtime
        .service()
        .advance_identity(sender.clone())
        .await
        .unwrap();

    let echo = PqUdpEchoBody {
        value_1: -1,
        value_2: i32::MAX,
    };
    send_request(
        &sender_socket,
        runtime.endpoints().p2p,
        sender.user_no.get(),
        0x1234_5678,
        UdpLogicalBody::PqUdpEcho(echo),
        20,
    )
    .await;
    let ingress = next_ingress(&mut runtime).await;
    assert_eq!(ingress.transport, UdpTransport::P2p);
    runtime
        .service()
        .dispatch(request(ingress, &sender, Vec::new()))
        .await
        .unwrap();
    let echo_reply = receive_ingress(&sender_socket, UdpTransport::P2p).await;
    assert_eq!(echo_reply.source, runtime.endpoints().p2p);
    assert_eq!(echo_reply.body, UdpIngressBody::PrUdpEcho(echo.reply()));

    let body = b"p2p-ingress-server-relay";
    send_request(
        &sender_socket,
        runtime.endpoints().p2p,
        sender.user_no.get(),
        0x0BAD_F00D,
        UdpLogicalBody::GameSlotPacket(body),
        21,
    )
    .await;
    let ingress = next_ingress(&mut runtime).await;
    let outcome = runtime
        .service()
        .dispatch(request(ingress, &sender, vec![target.clone()]))
        .await
        .unwrap();
    assert_eq!(outcome.sent_datagrams, 1);

    let relayed = receive_ingress(&target_socket, UdpTransport::P2p).await;
    assert_eq!(
        relayed.source,
        runtime.endpoints().p2p,
        "the ingress socket is also the relay source"
    );
    assert_eq!(relayed.account_id, target.user_no.get());
    assert_eq!(relayed.route_hash, 0xCAFE_BABE);
    assert_eq!(relayed.body, UdpIngressBody::GameSlotPacket(body.to_vec()));
    runtime.shutdown().await;
}

#[tokio::test]
async fn advance_and_delayed_release_fence_stale_generation_and_keep_new_endpoint() {
    let (mut runtime, old_socket, new_socket) = fixture().await;
    let sessions = session_ids(2).await;
    let mut identities = IdentityRegistry::new();
    let old = identities
        .claim(sessions[0], LOOPBACK, "Migrating")
        .unwrap();
    bind_with_echo(&mut runtime, &old_socket, &old, 1).await;

    let released = match identities.disconnect(sessions[0], Instant::now()) {
        DisconnectOutcome::Released(released) => released,
        outcome => panic!("expected immediate release, got {outcome:?}"),
    };
    let replacement = identities
        .claim(sessions[1], LOOPBACK, "mIGRATING")
        .unwrap();
    assert!(replacement.generation.get() > old.generation.get());
    let service = runtime.service();
    service.advance_identity(replacement.clone()).await.unwrap();

    send_request(
        &old_socket,
        runtime.endpoints().game,
        old.user_no.get(),
        2,
        UdpLogicalBody::PqUdpEcho(PqUdpEchoBody {
            value_1: 1,
            value_2: 2,
        }),
        30,
    )
    .await;
    let stale_ingress = next_ingress(&mut runtime).await;
    assert_eq!(
        service
            .dispatch(request(stale_ingress, &old, Vec::new()))
            .await,
        Err(UdpServiceError::EndpointState(
            UdpEndpointStateError::StaleGeneration {
                transport: UdpTransport::Game,
                account_id: old.user_no.get(),
                attempted_generation: old.generation.get(),
                current_generation: replacement.generation.get(),
            }
        ))
    );
    assert_no_datagram(&old_socket).await;

    bind_with_echo(&mut runtime, &new_socket, &replacement, 0xDEAD_BEEF).await;
    service.release_identity(released).await.unwrap();
    let current = service
        .current_target(UdpTransport::Game, replacement.clone())
        .await
        .unwrap()
        .expect("a stale release must retain the replacement endpoint");
    assert_eq!(current.endpoint.endpoint, new_socket.local_addr().unwrap());
    assert_eq!(current.endpoint.route_hash, 0xDEAD_BEEF);
    runtime.shutdown().await;
}

#[tokio::test]
async fn malformed_and_auth_failures_do_not_mutate_or_emit() {
    let (mut runtime, client, _) = fixture().await;
    client
        .send_to(&[1, 2, 3], runtime.endpoints().game)
        .await
        .unwrap();
    wait_for_malformed(&runtime).await;
    assert!(runtime.try_next_ingress().is_err());

    let sessions = session_ids(2).await;
    let mut identities = IdentityRegistry::new();
    let identity = identities
        .claim(sessions[0], LOOPBACK, "RoomRider")
        .unwrap();
    let other = identities
        .claim(sessions[1], LOOPBACK, "OtherRider")
        .unwrap();

    send_request(
        &client,
        runtime.endpoints().game,
        identity.user_no.get(),
        0x1111_0000,
        UdpLogicalBody::PqUdpEcho(PqUdpEchoBody {
            value_1: 1,
            value_2: 2,
        }),
        39,
    )
    .await;
    let mismatched = next_ingress(&mut runtime).await;
    assert_eq!(
        runtime
            .service()
            .dispatch(request(mismatched, &other, Vec::new()))
            .await,
        Err(UdpServiceError::IdentityMismatch {
            ingress_account_id: identity.user_no.get(),
            resolved_account_id: other.user_no.get(),
        })
    );
    assert!(
        runtime
            .service()
            .current_target(UdpTransport::Game, identity.clone())
            .await
            .unwrap()
            .is_none(),
        "failed authorization must not bind an endpoint"
    );
    assert_no_datagram(&client).await;
    runtime.shutdown().await;
}

#[tokio::test]
async fn room_and_client_replies_relay_and_audience_limits_are_bounded() {
    let config = UdpRuntimeConfig {
        maximum_relay_targets: 1,
        ..UdpRuntimeConfig::default()
    };
    let (mut runtime, client, other_client) = fixture_with_config(config).await;
    let sessions = session_ids(2).await;
    let mut identities = IdentityRegistry::new();
    let identity = identities
        .claim(sessions[0], LOOPBACK, "RoomRider")
        .unwrap();
    let other = identities
        .claim(sessions[1], LOOPBACK, "OtherRider")
        .unwrap();
    bind_with_echo(&mut runtime, &client, &identity, 0x4444_5555).await;
    bind_with_echo(&mut runtime, &other_client, &other, 0).await;

    let room_body = b"exact MyRoom relay";
    send_request(
        &client,
        runtime.endpoints().game,
        identity.user_no.get(),
        0x4444_5555,
        UdpLogicalBody::RoomSlotPacket(room_body),
        40,
    )
    .await;
    let ingress = next_ingress(&mut runtime).await;
    let mut room_request = request(ingress, &identity, Vec::new());
    room_request.room_targets = vec![other.clone()];
    let outcome = runtime.service().dispatch(room_request).await.unwrap();
    assert_eq!(outcome.action, UdpDispatchAction::RoomSlotRelay);
    assert_eq!(outcome.sent_datagrams, 1);
    assert_no_datagram(&client).await;
    let relayed = receive_ingress(&other_client, UdpTransport::Game).await;
    assert_eq!(relayed.source, runtime.endpoints().game);
    assert_eq!(relayed.account_id, other.user_no.get());
    assert_eq!(
        relayed.route_hash, 0x4444_5555,
        "a zero target route falls back to the source route"
    );
    assert_eq!(
        relayed.body,
        UdpIngressBody::RoomSlotPacket(room_body.to_vec())
    );

    for (iv, body) in [
        (
            42,
            UdpLogicalBody::PrUdpEcho(PrUdpEchoBody {
                value_1: -5,
                value_2: 6,
            }),
        ),
        (
            43,
            UdpLogicalBody::PrUdpTimeSync(PrUdpTimeSyncBody {
                client_tick: -7,
                server_tick: 8,
            }),
        ),
    ] {
        send_request(
            &client,
            runtime.endpoints().game,
            identity.user_no.get(),
            0x6666_7777,
            body,
            iv,
        )
        .await;
        let ingress = next_ingress(&mut runtime).await;
        let outcome = runtime
            .service()
            .dispatch(request(ingress, &identity, Vec::new()))
            .await
            .unwrap();
        assert_eq!(outcome.action, UdpDispatchAction::ClientReplyDropped);
        assert_eq!(outcome.sent_datagrams, 0);
        assert_no_datagram(&client).await;
    }

    send_request(
        &client,
        runtime.endpoints().game,
        identity.user_no.get(),
        9,
        UdpLogicalBody::GameSlotPacket(b"bounded"),
        41,
    )
    .await;
    let ingress = next_ingress(&mut runtime).await;
    assert_eq!(
        runtime
            .service()
            .dispatch(request(ingress, &identity, vec![other.clone(), other]))
            .await,
        Err(UdpServiceError::TooManyRelayTargets {
            actual: 2,
            maximum: 1,
        })
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn oversized_datagram_is_dropped_without_stopping_the_reader() {
    let config = UdpRuntimeConfig {
        maximum_payload: 64,
        ..UdpRuntimeConfig::default()
    };
    let (mut runtime, client, _) = fixture_with_config(config).await;
    let (_, identity) = active_identity("Oversized").await;
    runtime
        .service()
        .advance_identity(identity.clone())
        .await
        .unwrap();

    client
        .send_to(&vec![0xA5; 2_048], runtime.endpoints().game)
        .await
        .unwrap();
    wait_for_malformed(&runtime).await;

    let echo = PqUdpEchoBody {
        value_1: 51,
        value_2: 36,
    };
    send_request_with_maximum(
        &client,
        runtime.endpoints().game,
        identity.user_no.get(),
        0x5136_5136,
        UdpLogicalBody::PqUdpEcho(echo),
        51,
        config.maximum_payload,
    )
    .await;
    let ingress = next_ingress(&mut runtime).await;
    let outcome = runtime
        .service()
        .dispatch(request(ingress, &identity, Vec::new()))
        .await
        .unwrap();
    assert_eq!(outcome.action, UdpDispatchAction::EchoReply);
    assert_eq!(outcome.sent_datagrams, 1);

    let mut wire = vec![0_u8; 256];
    let (length, source) = timeout(TEST_TIMEOUT, client.recv_from(&mut wire))
        .await
        .expect("reader did not answer after oversized datagram")
        .unwrap();
    assert_eq!(source, runtime.endpoints().game);
    let reply = decode_udp_ingress(
        UdpTransport::Game,
        source,
        &wire[..length],
        config.maximum_payload,
    )
    .unwrap();
    assert_eq!(reply.body, UdpIngressBody::PrUdpEcho(echo.reply()));
    runtime.shutdown().await;
}

async fn fixture() -> (UdpRuntime, UdpSocket, UdpSocket) {
    fixture_with_config(UdpRuntimeConfig::default()).await
}

async fn fixture_with_config(config: UdpRuntimeConfig) -> (UdpRuntime, UdpSocket, UdpSocket) {
    let game_server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let p2p_server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let runtime = UdpRuntime::spawn(game_server, p2p_server, config).unwrap();
    let first_client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let second_client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    (runtime, first_client, second_client)
}

async fn active_identity(name: &str) -> (IdentityRegistry, IdentityBinding) {
    let sessions = session_ids(1).await;
    let mut identities = IdentityRegistry::new();
    let identity = identities.claim(sessions[0], LOOPBACK, name).unwrap();
    (identities, identity)
}

async fn session_ids(count: usize) -> Vec<SessionId> {
    let (world, world_task) =
        WorldHandle::spawn(count.max(1) + 1).expect("nonzero World mailbox capacity");
    let mut sessions = Vec::with_capacity(count);
    for index in 0..count {
        let port = u16::try_from(20_000 + index).unwrap();
        sessions.push(
            world
                .register_session(SocketAddr::new(LOOPBACK, port))
                .await
                .unwrap(),
        );
    }
    world.shutdown().await.unwrap();
    world_task.await.unwrap().unwrap();
    sessions
}

fn request(
    ingress: UdpIngress,
    identity: &IdentityBinding,
    racing_targets: Vec<IdentityBinding>,
) -> UdpDispatchRequest {
    UdpDispatchRequest {
        ingress,
        identity: identity.clone(),
        racing_targets,
        room_targets: Vec::new(),
    }
}

async fn bind_with_echo(
    runtime: &mut UdpRuntime,
    client: &UdpSocket,
    identity: &IdentityBinding,
    route_hash: u32,
) {
    runtime
        .service()
        .advance_identity(identity.clone())
        .await
        .unwrap();
    let echo = PqUdpEchoBody {
        value_1: 10,
        value_2: 20,
    };
    send_request(
        client,
        runtime.endpoints().game,
        identity.user_no.get(),
        route_hash,
        UdpLogicalBody::PqUdpEcho(echo),
        identity.user_no.get(),
    )
    .await;
    let ingress = next_ingress(runtime).await;
    runtime
        .service()
        .dispatch(request(ingress, identity, Vec::new()))
        .await
        .unwrap();
    assert_eq!(
        receive_ingress(client, UdpTransport::Game).await.body,
        UdpIngressBody::PrUdpEcho(echo.reply())
    );
}

async fn send_request(
    socket: &UdpSocket,
    endpoint: SocketAddr,
    account_id: u32,
    route_hash: u32,
    body: UdpLogicalBody<'_>,
    iv: u32,
) {
    send_request_with_maximum(
        socket,
        endpoint,
        account_id,
        route_hash,
        body,
        iv,
        DEFAULT_MAX_DATAGRAM_PAYLOAD,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn send_request_with_maximum(
    socket: &UdpSocket,
    endpoint: SocketAddr,
    account_id: u32,
    route_hash: u32,
    body: UdpLogicalBody<'_>,
    iv: u32,
    maximum_payload: usize,
) {
    let logical = encode_routed_udp_packet(&RoutedUdpPacket {
        account_id,
        route_hash,
        body,
    })
    .unwrap();
    let wire = encode_datagram(&logical, iv, maximum_payload).unwrap();
    socket.send_to(&wire, endpoint).await.unwrap();
}

async fn next_ingress(runtime: &mut UdpRuntime) -> UdpIngress {
    timeout(TEST_TIMEOUT, runtime.next_ingress())
        .await
        .expect("UDP ingress timed out")
        .expect("UDP admission queue closed")
}

async fn receive_ingress(socket: &UdpSocket, transport: UdpTransport) -> UdpIngress {
    let mut wire = vec![0_u8; 65_535];
    let (length, source) = timeout(TEST_TIMEOUT, socket.recv_from(&mut wire))
        .await
        .expect("UDP response timed out")
        .unwrap();
    decode_udp_ingress(
        transport,
        source,
        &wire[..length],
        DEFAULT_MAX_DATAGRAM_PAYLOAD,
    )
    .unwrap()
}

async fn assert_no_datagram(socket: &UdpSocket) {
    let mut wire = vec![0_u8; 65_535];
    match timeout(Duration::from_millis(75), socket.recv_from(&mut wire)).await {
        Err(_) => {}
        Ok(Ok((length, source))) => {
            panic!("unexpected {length}-byte UDP datagram from {source}");
        }
        Ok(Err(error)) => panic!("unexpected UDP receive error: {error}"),
    }
}

async fn wait_for_malformed(runtime: &UdpRuntime) {
    timeout(TEST_TIMEOUT, async {
        loop {
            if runtime.stats().malformed_dropped != 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("malformed datagram was not observed");
}
