use p5136_client_oracle::club::{
    ClubListCount, CreateCondition, Membership, PendingJoin, WaitingCrewCapacity,
    decode_club_list_count, decode_create_condition, decode_membership, decode_pending_join,
    decode_waiting_crew_capacity,
};
use p5136_core::club_query_protocol::{
    serialize_club_creation_unavailable_reply, serialize_empty_club_list_count_reply,
    serialize_no_club_state_reply, serialize_no_pending_club_join_reply,
    serialize_unavailable_waiting_crew_count_reply,
};

#[test]
fn empty_server_replies_take_the_evidenced_fail_closed_client_branches() {
    assert_eq!(
        decode_membership(&serialize_no_club_state_reply().unwrap()).unwrap(),
        Membership::NoClub
    );
    assert_eq!(
        decode_pending_join(&serialize_no_pending_club_join_reply().unwrap()).unwrap(),
        PendingJoin::None
    );
    assert_eq!(
        decode_create_condition(&serialize_club_creation_unavailable_reply()).unwrap(),
        CreateCondition::Unavailable
    );
    assert_eq!(
        decode_club_list_count(&serialize_empty_club_list_count_reply()).unwrap(),
        ClubListCount::LocalPageFallback
    );
    assert_eq!(
        decode_waiting_crew_capacity(&serialize_unavailable_waiting_crew_count_reply()).unwrap(),
        WaitingCrewCapacity::FullOrUnavailable {
            current: 0,
            capacity: 0
        }
    );
}

#[test]
fn independent_packets_exercise_both_sides_of_every_consumer_gate() {
    assert_eq!(
        decode_membership(&membership_packet(0x1020_3040)).unwrap(),
        Membership::Club(0x1020_3040)
    );
    assert_eq!(
        decode_pending_join(&pending_join_packet(0, 77)).unwrap(),
        PendingJoin::LookupFailed
    );
    assert_eq!(
        decode_pending_join(&pending_join_packet(1, 77)).unwrap(),
        PendingJoin::Club(77)
    );

    for (status, expected) in [
        (0, CreateCondition::Allowed),
        (1, CreateCondition::InsufficientRp),
        (2, CreateCondition::InsufficientLucci),
        (3, CreateCondition::Unavailable),
        (4, CreateCondition::RefreshRequired),
        (99, CreateCondition::Unknown(99)),
    ] {
        assert_eq!(
            decode_create_condition(&word_packet(0xC998_0C79, &[status])).unwrap(),
            expected
        );
    }

    assert_eq!(
        decode_club_list_count(&word_packet(0x72E0_0965, &[7, 123])).unwrap(),
        ClubListCount::Count(7)
    );
    assert_eq!(
        decode_waiting_crew_capacity(&word_packet(0xBF7C_0C2D, &[2, 3])).unwrap(),
        WaitingCrewCapacity::CanJoin {
            current: 2,
            capacity: 3
        }
    );
    assert_eq!(
        decode_waiting_crew_capacity(&word_packet(0xBF7C_0C2D, &[3, 3])).unwrap(),
        WaitingCrewCapacity::FullOrUnavailable {
            current: 3,
            capacity: 3
        }
    );
}

fn membership_packet(club_code: u32) -> Vec<u8> {
    let mut packet = word_packet(0x718B_0945, &[club_code]);
    push_utf16(&mut packet, "Club");
    push_u32(&mut packet, 11);
    push_u32(&mut packet, 22);
    packet.extend_from_slice(&33_u16.to_le_bytes());
    push_utf16(&mut packet, "Master");
    push_u32(&mut packet, 44);
    packet.push(55);
    packet
}

fn pending_join_packet(status: u32, club_code: u32) -> Vec<u8> {
    let mut packet = word_packet(0xB4E2_0BC2, &[status, club_code]);
    push_utf16(&mut packet, "Pending");
    packet
}

fn word_packet(hash: u32, words: &[u32]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(4 + words.len() * 4);
    push_u32(&mut packet, hash);
    for word in words {
        push_u32(&mut packet, *word);
    }
    packet
}

fn push_utf16(packet: &mut Vec<u8>, value: &str) {
    let units = value.encode_utf16().collect::<Vec<_>>();
    push_u32(packet, u32::try_from(units.len()).unwrap());
    for unit in units {
        packet.extend_from_slice(&unit.to_le_bytes());
    }
}

fn push_u32(packet: &mut Vec<u8>, value: u32) {
    packet.extend_from_slice(&value.to_le_bytes());
}
