use std::{array, net::Ipv4Addr};

use p5136_client_oracle::room::{
    Member, decode_create_room_reply, decode_join_room_reply, decode_room_list,
    decode_session_data, decode_slot_data,
};
use p5136_core::room_protocol::{
    CreateRoomOutcome, JoinRoomStatus, RoomAi, RoomListEntry, RoomMember, RoomObserver,
    RoomObserverSlot, RoomPlayer, RoomSessionData, RoomSlotData, serialize_ch_create_room_reply,
    serialize_ch_get_room_list_reply, serialize_ch_join_room_reply, serialize_gr_session_data,
    serialize_gr_slot_data,
};

#[test]
fn structural_room_list_reader_preserves_all_client_visible_fields() {
    let rooms = [
        RoomListEntry {
            room_id: 17,
            room_name: "첫방".to_owned(),
            track: 0x1020_3040,
            locked: true,
            game_type: 4,
            speed_type: 7,
            started: false,
            available_slots: 5,
            player_count: 3,
        },
        RoomListEntry {
            room_id: 29,
            room_name: "B".to_owned(),
            track: 0x5060_7080,
            locked: false,
            game_type: 1,
            speed_type: 2,
            started: true,
            available_slots: 1,
            player_count: 7,
        },
    ];
    let packet = serialize_ch_get_room_list_reply(123, &rooms).unwrap();
    let decoded = decode_room_list(&packet).unwrap();

    assert_eq!(decoded.page, 123);
    assert_eq!(decoded.rooms.len(), 2);
    assert_eq!(decoded.rooms[0].room_id, 17);
    assert_eq!(decoded.rooms[0].room_name, "첫방");
    assert_eq!(decoded.rooms[0].track, 0x1020_3040);
    assert!(decoded.rooms[0].locked);
    assert_eq!(decoded.rooms[0].game_type, 4);
    assert_eq!(decoded.rooms[0].available_slots, 5);
    assert_eq!(decoded.rooms[1].room_id, 29);
    assert!(decoded.rooms[1].started);
    assert_eq!(decoded.rooms[1].player_count, 7);
}

#[test]
fn room_admission_reader_exercises_status_and_game_type_branches() {
    for game_type in [0, 2, 3, 4, 9, u8::MAX] {
        let created = decode_create_room_reply(&serialize_ch_create_room_reply(
            CreateRoomOutcome::Created,
            game_type,
        ))
        .unwrap();
        assert!(created.created);
        assert!(created.echoed_created);
        let client_game_type = if game_type == u8::MAX { 0 } else { game_type };
        assert_eq!(created.game_type, client_game_type);
        assert_eq!(
            created.slot_hint,
            if matches!(game_type, 3 | 4) { 2 } else { 8 }
        );

        let rejected = decode_create_room_reply(&serialize_ch_create_room_reply(
            CreateRoomOutcome::Rejected,
            game_type,
        ))
        .unwrap();
        assert!(!rejected.created);
        assert_eq!(rejected.slot_hint, 0);
    }

    for (status, expected_code) in [
        (JoinRoomStatus::Success, 0),
        (JoinRoomStatus::Unavailable, 1),
        (JoinRoomStatus::Full, 2),
        (JoinRoomStatus::WrongPassword, 3),
    ] {
        let reply = decode_join_room_reply(&serialize_ch_join_room_reply(status, 4)).unwrap();
        assert_eq!(reply.status, expected_code);
        assert_eq!(reply.success, expected_code == 0);
        assert_eq!(reply.slot_hint, if expected_code == 0 { 2 } else { 0 });
        assert_eq!(reply.game_type, 4);
        assert_eq!(reply.terminal, 0);
    }
}

#[test]
fn initial_room_snapshot_is_consumed_without_server_reader_reuse() {
    let session = RoomSessionData {
        room_name: "LAN 테스트".to_owned(),
        password: "pw".to_owned(),
        game_type: 4,
        speed_type: 6,
    };
    let mut slots = RoomSlotData::empty(
        0x1020_3040,
        0xAABB_CCDD,
        array::from_fn(|index| u8::try_from(index).unwrap()),
        0,
    );
    slots.closed_slot_ids.push(1);
    slots.members_by_id[0] = RoomMember::Player(RoomPlayer {
        player_type: 2,
        user_no: 0x0102_0304,
        p2p_address: Ipv4Addr::new(192, 168, 1, 15),
        p2p_port: 39_312,
        nickname: "Rider가".to_owned(),
        emblem_1: 0x1234,
        emblem_2: 0x5678,
        emblem_3: 0x9ABC,
        rider_item_snapshot: array::from_fn(|index| {
            0x80_u8.wrapping_add(u8::try_from(index).unwrap())
        }),
        card: "CARD".to_owned(),
        rp: 123_456,
        team: 2,
        ranking: 77,
        rider_school_level: 6,
        club_name: "클럽".to_owned(),
        club_mark_logo: 0x1122_3344,
    });
    slots.members_by_id[1] = RoomMember::Closed { player_type: 1 };
    slots.members_by_id[2] = RoomMember::Ai(RoomAi {
        character: 11,
        rider: 12,
        kart: 13,
        balloon: 14,
        head_band: 15,
        goggle: 16,
        team: 1,
    });
    slots.observers[0] = RoomObserverSlot::Player(RoomObserver {
        player_type: 4,
        user_no: 0x5566_7788,
        p2p_address: Ipv4Addr::new(10, 20, 30, 40),
        p2p_port: 40_000,
        nickname: "Observer".to_owned(),
    });
    slots.slot_positions[0] = 0;
    slots.slot_positions[2] = 1;

    let session_packet = serialize_gr_session_data(&session).unwrap();
    let slot_packet = serialize_gr_slot_data(&slots).unwrap();
    let decoded_session = decode_session_data(&session_packet).unwrap();
    let decoded_slots = decode_slot_data(&slot_packet).unwrap();

    assert_eq!(decoded_session.room_name, "LAN 테스트");
    assert_eq!(decoded_session.password, "pw");
    assert_eq!(decoded_session.game_type, 4);
    assert_eq!(decoded_slots.track, 0x1020_3040);
    assert_eq!(decoded_slots.room_data_header, 0xAABB_CCDD);
    assert_eq!(decoded_slots.room_master, 0);
    assert_eq!(decoded_slots.closed_slot_ids, [1]);
    let Member::Player(player) = &decoded_slots.members[0] else {
        panic!("slot zero did not decode as a player");
    };
    assert_eq!(player.user_no, 0x0102_0304);
    assert_eq!(player.p2p.address, Ipv4Addr::new(192, 168, 1, 15));
    assert_eq!(player.p2p.port, 39_312);
    assert_eq!(player.nickname, "Rider가");
    assert_eq!(player.emblems, [0x1234, 0x5678, 0x9ABC]);
    assert_eq!(player.rider_items[0], 0x80);
    assert_eq!(player.rider_items[64], 0xC0);
    assert_eq!(player.club_name, "클럽");
    assert_eq!(player.club_mark_logo, 0x1122_3344);
    assert!(matches!(
        decoded_slots.members[1],
        Member::Closed { player_type: 1 }
    ));
    let Member::Ai(ai) = decoded_slots.members[2] else {
        panic!("slot two did not decode as AI");
    };
    assert_eq!(ai.kart, 13);
    assert_eq!(ai.team, 1);
    let observer = decoded_slots.observers[0].as_ref().unwrap();
    assert_eq!(observer.user_no, 0x5566_7788);
    assert_eq!(observer.nickname, "Observer");
    assert_eq!(decoded_slots.slot_positions[0], 0);
    assert_eq!(decoded_slots.slot_positions[2], 1);

    for length in 0..slot_packet.len() {
        assert!(decode_slot_data(&slot_packet[..length]).is_err());
    }
}
