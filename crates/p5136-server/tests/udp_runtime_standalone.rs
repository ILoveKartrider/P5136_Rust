use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
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
async fn four_clients_keep_independent_sender_ticks_relayable() {
    let game_server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let p2p_server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut runtime =
        UdpRuntime::spawn(game_server, p2p_server, UdpRuntimeConfig::default()).unwrap();
    let mut clients = Vec::with_capacity(4);
    for _ in 0..4 {
        clients.push(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());
    }

    let sessions = session_ids(4).await;
    let mut registry = IdentityRegistry::new();
    let identities = sessions
        .into_iter()
        .enumerate()
        .map(|(index, session)| {
            registry
                .claim(session, LOOPBACK, &format!("TickRider{index}"))
                .unwrap()
        })
        .collect::<Vec<_>>();
    for (index, (client, identity)) in clients.iter().zip(&identities).enumerate() {
        bind_with_echo(
            &mut runtime,
            client,
            identity,
            0x5100_0000 + u32::try_from(index).unwrap(),
        )
        .await;
    }

    // A fourth client observes three independent PCs. Their uptime-derived
    // movement ticks intentionally descend, which is valid because ticks are
    // only ordered within one sender timeline.
    let independent_sender_ticks = [900_000_u32, 600_000, 300_000];
    for (sender_index, tick) in independent_sender_ticks.into_iter().enumerate() {
        let body = movement_game_slot_body(sender_index, tick);
        send_request(
            &clients[sender_index],
            runtime.endpoints().game,
            identities[sender_index].user_no.get(),
            0x5200_0000 + u32::try_from(sender_index).unwrap(),
            UdpLogicalBody::GameSlotPacket(&body),
            100 + u32::try_from(sender_index).unwrap(),
        )
        .await;
        let ingress = next_ingress(&mut runtime).await;
        let outcome = runtime
            .service()
            .dispatch(request(
                ingress,
                &identities[sender_index],
                identities.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(outcome.action, UdpDispatchAction::GameSlotRelay);
        assert_eq!(outcome.sent_datagrams, 3);
        assert_eq!(outcome.failed_sends, 0);
        assert_eq!(outcome.unavailable_targets, 0);

        for (target_index, client) in clients.iter().enumerate() {
            if target_index == sender_index {
                assert_no_datagram(client).await;
                continue;
            }
            let relayed = receive_ingress(client, UdpTransport::Game).await;
            assert_eq!(relayed.account_id, identities[target_index].user_no.get());
            assert_eq!(
                relayed.body,
                UdpIngressBody::GameSlotPacket(body.clone()),
                "receiver {target_index} lost sender {sender_index} tick {tick}"
            );
        }
    }

    runtime.shutdown().await;
}

#[tokio::test]
async fn eight_clients_each_relay_exact_movement_to_the_other_seven() {
    const CLIENT_COUNT: usize = 8;

    let game_server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let p2p_server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut runtime =
        UdpRuntime::spawn(game_server, p2p_server, UdpRuntimeConfig::default()).unwrap();
    let mut clients = Vec::with_capacity(CLIENT_COUNT);
    for _ in 0..CLIENT_COUNT {
        clients.push(Arc::new(
            UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap(),
        ));
    }

    let sessions = session_ids(CLIENT_COUNT).await;
    let mut registry = IdentityRegistry::new();
    let identities = sessions
        .into_iter()
        .enumerate()
        .map(|(index, session)| {
            registry
                .claim(session, LOOPBACK, &format!("EightRider{index}"))
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut current_route_hashes = (0..CLIENT_COUNT)
        .map(|index| 0x7100_0000 + u32::try_from(index).unwrap())
        .collect::<Vec<_>>();
    for ((client, identity), route_hash) in clients
        .iter()
        .zip(&identities)
        .zip(current_route_hashes.iter().copied())
    {
        bind_with_echo(&mut runtime, client, identity, route_hash).await;
    }

    // These ticks deliberately have no global ordering. Each stock client has
    // its own uptime-derived movement timeline, so only one sender's sequence
    // may be compared with itself.
    let sender_ticks = [
        900_000_u32,
        7,
        u32::MAX - 1,
        300_000,
        42,
        800_000,
        1,
        600_000,
    ];
    for (sender_index, tick) in sender_ticks.into_iter().enumerate() {
        let body = movement_game_slot_body(sender_index, tick);
        let relay_route_hash = 0x7200_0000 + u32::try_from(sender_index).unwrap();
        send_request(
            &clients[sender_index],
            runtime.endpoints().game,
            identities[sender_index].user_no.get(),
            relay_route_hash,
            UdpLogicalBody::GameSlotPacket(&body),
            0x5136_0100 + u32::try_from(sender_index).unwrap(),
        )
        .await;
        let ingress = next_ingress(&mut runtime).await;
        current_route_hashes[sender_index] = relay_route_hash;
        let outcome = runtime
            .service()
            .dispatch(request(
                ingress,
                &identities[sender_index],
                identities.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(outcome.action, UdpDispatchAction::GameSlotRelay);
        assert_eq!(outcome.sent_datagrams, CLIENT_COUNT - 1);
        assert_eq!(outcome.failed_sends, 0);
        assert_eq!(outcome.unavailable_targets, 0);

        for (target_index, client) in clients.iter().enumerate() {
            if target_index == sender_index {
                assert_no_datagram(client).await;
                continue;
            }
            let relayed = receive_ingress(client, UdpTransport::Game).await;
            assert_eq!(relayed.source, runtime.endpoints().game);
            assert_eq!(relayed.account_id, identities[target_index].user_no.get());
            assert_eq!(
                relayed.route_hash, current_route_hashes[target_index],
                "receiver {target_index} did not use its latest route after sender {sender_index}"
            );
            assert_eq!(
                relayed.body,
                UdpIngressBody::GameSlotPacket(body.clone()),
                "receiver {target_index} lost sender {sender_index} tick {tick}"
            );
        }
    }

    runtime.shutdown().await;
}

#[tokio::test]
#[ignore = "runs an eight-client UDP relay stress loop for two minutes"]
#[allow(
    clippy::too_many_lines,
    reason = "the stress scenario keeps setup, timed traffic, and exact relay assertions together"
)]
async fn eight_clients_sustain_jittered_exact_relay_for_configured_duration() {
    const CLIENT_COUNT: usize = 8;

    let game_server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let p2p_server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut runtime =
        UdpRuntime::spawn(game_server, p2p_server, UdpRuntimeConfig::default()).unwrap();
    let mut clients = Vec::with_capacity(CLIENT_COUNT);
    for _ in 0..CLIENT_COUNT {
        clients.push(Arc::new(
            UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap(),
        ));
    }

    let sessions = session_ids(CLIENT_COUNT).await;
    let mut registry = IdentityRegistry::new();
    let identities = sessions
        .into_iter()
        .enumerate()
        .map(|(index, session)| {
            registry
                .claim(session, LOOPBACK, &format!("StressRider{index}"))
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut current_route_hashes = (0..CLIENT_COUNT)
        .map(|index| 0x7300_0000 + u32::try_from(index).unwrap())
        .collect::<Vec<_>>();
    for ((client, identity), route_hash) in clients
        .iter()
        .zip(&identities)
        .zip(current_route_hashes.iter().copied())
    {
        bind_with_echo(&mut runtime, client, identity, route_hash).await;
    }

    // Sender-local clocks intentionally begin far apart, including near wrap.
    // The relay must preserve each opaque movement payload without imposing a
    // single server-global tick ordering on eight independent clients.
    let mut sender_ticks = [
        900_000_u32,
        7,
        u32::MAX - 4_096,
        300_000,
        42,
        800_000,
        1,
        600_000,
    ];
    let mut last_sent_movement_ticks: [Option<u32>; CLIENT_COUNT] = [None; CLIENT_COUNT];
    let mut per_sender_dispatches = [0_u64; CLIENT_COUNT];
    let mut per_sender_requests = [0_u64; CLIENT_COUNT];
    let mut forced_sender_cursor = 0_usize;
    let mut movement_dispatch_count = 0_u64;
    let mut echo_dispatch_count = 0_u64;
    let mut out_of_order_movement_count = 0_u64;
    let mut arrival_reorder_count = 0_u64;
    let mut request_count = 0_u64;
    let mut relay_datagram_count = 0_u64;
    let mut random_state = 0x5136_8a11_5eed_cafe_u64;
    let duration = udp_stress_duration();
    let started = Instant::now();
    let deadline = started + duration;

    while Instant::now() < deadline {
        let jitter = next_udp_stress_jitter(&mut random_state);
        if !jitter.is_zero() {
            tokio::time::sleep(jitter.min(deadline.saturating_duration_since(Instant::now())))
                .await;
        }
        if Instant::now() >= deadline {
            break;
        }

        // Queue a small burst before draining ingress. Requests from different
        // sockets, ping echoes, and deliberately stale movement ticks can then
        // interleave. Validation below is multiset-based and never assumes
        // receive order, while still requiring every exact datagram once.
        let batch_size = usize::try_from((next_stress_random(&mut random_state) % 11) + 2).unwrap();
        let mut send_tasks = Vec::with_capacity(batch_size);
        let mut batch_request_orders = Vec::with_capacity(batch_size);
        for operation_index in 0..batch_size {
            let force_movement = operation_index == 0;
            let is_echo = operation_index == 1
                || (!force_movement && next_stress_random(&mut random_state).is_multiple_of(4));
            let sender_index = if force_movement {
                let sender_index = forced_sender_cursor;
                forced_sender_cursor = (forced_sender_cursor + 1) % CLIENT_COUNT;
                sender_index
            } else {
                usize::try_from(next_stress_random(&mut random_state) % CLIENT_COUNT as u64)
                    .unwrap()
            };
            let sender_sequence = u32::try_from(per_sender_requests[sender_index]).unwrap();
            let route_hash = 0x7400_0000
                | (u32::try_from(sender_index).unwrap() << 20)
                | (sender_sequence & 0x000f_ffff);
            let iv = 0x5136_1000_u32.wrapping_add(u32::try_from(request_count).unwrap());
            let creation_order = request_count;
            let simulated_latency = if operation_index == 0 {
                Duration::from_millis(30)
            } else if operation_index == 1 {
                Duration::ZERO
            } else {
                Duration::from_millis(
                    u64::try_from(sender_index).unwrap() * 3
                        + (next_stress_random(&mut random_state) % 26),
                )
            };
            let socket = Arc::clone(&clients[sender_index]);
            let endpoint = runtime.endpoints().game;
            let account_id = identities[sender_index].user_no.get();

            if is_echo {
                let echo = PqUdpEchoBody {
                    value_1: i32::try_from(sender_index).unwrap(),
                    value_2: i32::try_from(request_count).unwrap(),
                };
                send_tasks.push(tokio::spawn(async move {
                    tokio::time::sleep(simulated_latency).await;
                    send_request(
                        &socket,
                        endpoint,
                        account_id,
                        route_hash,
                        UdpLogicalBody::PqUdpEcho(echo),
                        iv,
                    )
                    .await;
                }));
            } else {
                let tick_delta =
                    u32::try_from((next_stress_random(&mut random_state) % 2_048) + 1).unwrap();
                sender_ticks[sender_index] = sender_ticks[sender_index].wrapping_add(tick_delta);
                let stale = last_sent_movement_ticks[sender_index].is_some()
                    && (next_stress_random(&mut random_state).is_multiple_of(5)
                        || request_count.is_multiple_of(17));
                let tick = if let (true, Some(last_tick)) =
                    (stale, last_sent_movement_ticks[sender_index])
                {
                    out_of_order_movement_count += 1;
                    last_tick.wrapping_sub(
                        u32::try_from((next_stress_random(&mut random_state) % 4_096) + 1).unwrap(),
                    )
                } else {
                    sender_ticks[sender_index]
                };
                last_sent_movement_ticks[sender_index] = Some(tick);
                let body = movement_game_slot_body(sender_index, tick);
                send_tasks.push(tokio::spawn(async move {
                    tokio::time::sleep(simulated_latency).await;
                    send_request(
                        &socket,
                        endpoint,
                        account_id,
                        route_hash,
                        UdpLogicalBody::GameSlotPacket(&body),
                        iv,
                    )
                    .await;
                }));
            }

            batch_request_orders.push((iv, creation_order));
            per_sender_requests[sender_index] += 1;
            request_count += 1;
        }
        for send_task in send_tasks {
            send_task.await.unwrap();
        }

        let mut expected_by_client = vec![Vec::<(u32, UdpIngressBody)>::new(); CLIENT_COUNT];
        let mut batch_arrival_orders = Vec::with_capacity(batch_size);
        for _ in 0..batch_size {
            let ingress = next_ingress(&mut runtime).await;
            let creation_order = batch_request_orders
                .iter()
                .find_map(|(iv, order)| (*iv == ingress.iv).then_some(*order))
                .expect("stress ingress IV must identify one planned request");
            batch_arrival_orders.push(creation_order);
            let sender_index = identities
                .iter()
                .position(|identity| identity.user_no.get() == ingress.account_id)
                .expect("stress ingress must belong to one of the eight clients");
            current_route_hashes[sender_index] = ingress.route_hash;
            let ingress_route_hash = ingress.route_hash;
            let ingress_body = ingress.body.clone();
            let outcome = runtime
                .service()
                .dispatch(request(
                    ingress,
                    &identities[sender_index],
                    identities.clone(),
                ))
                .await
                .unwrap();

            match ingress_body {
                UdpIngressBody::PqUdpEcho(echo) => {
                    assert_eq!(outcome.action, UdpDispatchAction::EchoReply);
                    assert_eq!(outcome.sent_datagrams, 1);
                    expected_by_client[sender_index]
                        .push((ingress_route_hash, UdpIngressBody::PrUdpEcho(echo.reply())));
                    echo_dispatch_count += 1;
                }
                UdpIngressBody::GameSlotPacket(body) => {
                    assert_eq!(outcome.action, UdpDispatchAction::GameSlotRelay);
                    assert_eq!(outcome.sent_datagrams, CLIENT_COUNT - 1);
                    for target_index in 0..CLIENT_COUNT {
                        if target_index != sender_index {
                            expected_by_client[target_index].push((
                                current_route_hashes[target_index],
                                UdpIngressBody::GameSlotPacket(body.clone()),
                            ));
                        }
                    }
                    per_sender_dispatches[sender_index] += 1;
                    movement_dispatch_count += 1;
                    relay_datagram_count += u64::try_from(outcome.sent_datagrams).unwrap();
                }
                body => panic!("unexpected stress ingress body: {body:?}"),
            }
            assert_eq!(outcome.failed_sends, 0);
            assert_eq!(outcome.unavailable_targets, 0);
        }
        arrival_reorder_count += u64::try_from(
            batch_arrival_orders
                .windows(2)
                .filter(|pair| pair[1] < pair[0])
                .count(),
        )
        .unwrap();

        for (target_index, client) in clients.iter().enumerate() {
            let expected = &mut expected_by_client[target_index];
            for _ in 0..expected.len() {
                let received = receive_ingress(client, UdpTransport::Game).await;
                assert_eq!(received.source, runtime.endpoints().game);
                assert_eq!(received.account_id, identities[target_index].user_no.get());
                let Some(position) = expected.iter().position(|(route_hash, body)| {
                    *route_hash == received.route_hash && body == &received.body
                }) else {
                    panic!(
                        "client {target_index} received unexpected or duplicate datagram: route={:#010x}, body={:?}, remaining={expected:?}",
                        received.route_hash, received.body
                    );
                };
                expected.swap_remove(position);
            }
            assert!(expected.is_empty());
        }
    }

    assert!(
        movement_dispatch_count >= CLIENT_COUNT as u64,
        "stress duration was too short to exercise every sender"
    );
    assert_eq!(
        relay_datagram_count,
        movement_dispatch_count * (CLIENT_COUNT as u64 - 1)
    );
    assert!(per_sender_dispatches.iter().all(|count| *count != 0));
    assert!(
        echo_dispatch_count != 0,
        "stress run did not interleave ping echoes"
    );
    assert!(
        out_of_order_movement_count != 0,
        "stress run did not inject a stale movement tick"
    );
    assert!(
        arrival_reorder_count != 0,
        "different simulated client latencies did not reverse any ingress order"
    );
    for client in &clients {
        assert_no_datagram(client).await;
    }

    eprintln!(
        "8-client UDP stress: elapsed={:?}, movements={movement_dispatch_count}, echoes={echo_dispatch_count}, stale_ticks={out_of_order_movement_count}, arrival_reorders={arrival_reorder_count}, relayed_datagrams={relay_datagram_count}, per_sender={per_sender_dispatches:?}",
        started.elapsed()
    );
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

fn movement_game_slot_body(sender_index: usize, tick: u32) -> Vec<u8> {
    const GAME_KART_PACKET: u32 = 656_737_636;
    let mut body = vec![0_u8; 20];
    body[0] = u8::try_from(sender_index).unwrap();
    body.extend_from_slice(&GAME_KART_PACKET.to_le_bytes());
    body.extend_from_slice(&tick.to_le_bytes());
    body
}

fn udp_stress_duration() -> Duration {
    std::env::var("P5136_UDP_STRESS_SECONDS").map_or(Duration::from_secs(120), |value| {
        let seconds = value
            .parse::<u64>()
            .expect("P5136_UDP_STRESS_SECONDS must be a positive integer");
        assert!(seconds != 0, "P5136_UDP_STRESS_SECONDS must be positive");
        Duration::from_secs(seconds)
    })
}

fn next_stress_random(state: &mut u64) -> u64 {
    let mut value = *state;
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    *state = value;
    value
}

fn next_udp_stress_jitter(state: &mut u64) -> Duration {
    let value = next_stress_random(state);
    let bucket = value % 100;
    let milliseconds = match bucket {
        0..=24 => 0,
        25..=74 => 1 + ((value >> 8) % 12),
        75..=94 => 13 + ((value >> 8) % 28),
        _ => 50 + ((value >> 8) % 71),
    };
    Duration::from_millis(milliseconds)
}
