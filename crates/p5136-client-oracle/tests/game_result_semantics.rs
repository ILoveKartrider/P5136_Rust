use p5136_client_oracle::{
    ceremony::{Ceremony, CeremonyState},
    game_result,
};
use p5136_core::{
    race_protocol::{ServerGameControl, serialize_game_control, serialize_game_next_stage},
    race_result_protocol::{
        AiRaceResult, GameResult, HumanRaceResult, ResultTeam, serialize_game_result,
    },
};

#[test]
fn idb_fixture_is_consumed_without_calling_the_server_serializer() {
    let packet = decode_hex(include_str!("fixtures/game_result_idb_synthetic.hex"));
    let decoded = game_result::decode(&packet).unwrap();

    assert_eq!(decoded.winning_team, 1);
    assert_eq!(decoded.humans.len(), 1);
    let human = &decoded.humans[0];
    assert_eq!(human.player_id, 2);
    assert_eq!(human.finish_time, 0x1234_5678);
    assert_eq!(human.kart_id, 1_401);
    assert_eq!(human.rank, 0);
    assert_eq!(human.team_mode_marker, 2);
    assert_eq!(human.current_rp, 900);
    assert_eq!(human.earned_rp, 12);
    assert_eq!(human.earned_lucci, 34);
    assert_eq!(human.current_lucci, 5_678);
    assert_eq!(human.team, 1);
    assert_eq!(human.team_points, 10);
    assert_eq!(human.result_marker, 1);
    assert_eq!(human.character_id, 42);
    assert_eq!(human.display_marker, u8::MAX);
    assert_eq!(human.club_mark_logo, -7);

    assert_eq!(decoded.ais.len(), 1);
    let ai = &decoded.ais[0];
    assert_eq!(ai.player_id, 5);
    assert_eq!(ai.finish_time, u32::MAX);
    assert_eq!(ai.kart_id, 1_410);
    assert_eq!(ai.rank, 1);
    assert_eq!(ai.team_mode_marker, 0);
    assert_eq!(ai.team, 2);
    assert_eq!(ai.team_points, 0);
    assert_eq!(decoded.terminal_marker, u32::MAX);
    assert_eq!(decoded.terminal_status, 0);
}

#[test]
fn idb_next_stage_fixture_reaches_the_recovered_stage_transition_fields() {
    let packet = decode_hex(include_str!("fixtures/game_next_stage_idb.hex"));
    let decoded = p5136_client_oracle::ceremony::decode_game_next_stage(&packet).unwrap();
    assert_eq!(decoded.game_type, 4);
    assert_eq!(decoded.stage_marker_1, 0);
    assert_eq!(decoded.stage_marker_2, 0);
}

#[test]
fn reconstructed_client_reads_distinct_human_ai_and_team_semantics() {
    let humans = [
        HumanRaceResult {
            player_id: 2,
            finish_time: 0x1234_5678,
            kart_id: 1_401,
            rank: 0,
            current_rp: 90_001,
            earned_rp: 12,
            earned_lucci: 34,
            current_lucci: 56_789,
            team: Some(ResultTeam::Red),
            team_points: 10,
            character_id: 42,
            club_mark_logo: -7,
        },
        HumanRaceResult {
            player_id: 6,
            finish_time: 0x8765_4321,
            kart_id: 1_987,
            rank: 1,
            current_rp: 80_002,
            earned_rp: 21,
            earned_lucci: 43,
            current_lucci: 98_765,
            team: Some(ResultTeam::Blue),
            team_points: 4,
            character_id: 314,
            club_mark_logo: 0x1020_3040,
        },
    ];
    let ais = [AiRaceResult {
        player_id: 7,
        finish_time: u32::MAX,
        kart_id: 1_410,
        rank: 2,
        team: Some(ResultTeam::Red),
        team_points: 0,
    }];
    let packet = serialize_game_result(&GameResult {
        winning_team: Some(ResultTeam::Red),
        humans: &humans,
        ais: &ais,
    })
    .unwrap();

    let decoded = game_result::decode(&packet).unwrap();
    assert_eq!(decoded.winning_team, 1);
    assert_eq!(decoded.humans.len(), 2);
    assert_eq!(decoded.ais.len(), 1);
    assert_eq!(decoded.humans[0].player_id, 2);
    assert_eq!(decoded.humans[0].team, 1);
    assert_eq!(decoded.humans[0].team_points, 10);
    assert_eq!(decoded.humans[0].character_id, 42);
    assert_eq!(decoded.humans[0].club_mark_logo, -7);
    assert_eq!(decoded.humans[1].player_id, 6);
    assert_eq!(decoded.humans[1].team, 2);
    assert_eq!(decoded.humans[1].team_points, 4);
    assert_eq!(decoded.humans[1].character_id, 314);
    assert_eq!(decoded.humans[1].club_mark_logo, 0x1020_3040);
    assert_eq!(decoded.ais[0].player_id, 7);
    assert_eq!(decoded.ais[0].finish_time, u32::MAX);
    assert_eq!(decoded.ais[0].team, 1);
    assert_eq!(decoded.terminal_marker, u32::MAX);
    assert_eq!(decoded.terminal_status, 0);
}

#[test]
fn reconstructed_client_consumes_the_terminal_empty_result() {
    let packet = serialize_game_result(&GameResult {
        winning_team: None,
        humans: &[],
        ais: &[],
    })
    .unwrap();
    let decoded = game_result::decode(&packet).unwrap();
    assert_eq!(decoded.winning_team, 0);
    assert!(decoded.humans.is_empty());
    assert!(decoded.ais.is_empty());
    assert_eq!(decoded.terminal_marker, u32::MAX);
    assert_eq!(decoded.terminal_status, 0);
}

#[test]
fn reconstructed_reader_rejects_every_truncated_prefix_and_suffix_drift() {
    let humans = [solo_human()];
    let packet = serialize_game_result(&GameResult {
        winning_team: None,
        humans: &humans,
        ais: &[],
    })
    .unwrap();

    for length in 0..packet.len() {
        assert!(
            game_result::decode(&packet[..length]).is_err(),
            "truncated prefix of {length} bytes was accepted"
        );
    }
    let mut extended = packet;
    extended.push(0xA5);
    assert!(game_result::decode(&extended).is_err());
}

#[test]
fn old_csharp_217_byte_team_record_is_not_blessed_as_a_golden() {
    let humans = [solo_human()];
    let current = serialize_game_result(&GameResult {
        winning_team: None,
        humans: &humans,
        ais: &[],
    })
    .unwrap();
    let malformed = convert_to_old_csharp_record(&current);

    assert_eq!(malformed.len(), current.len() + 5);
    assert!(game_result::decode(&malformed).is_err());
}

#[test]
fn ceremony_oracle_requires_the_deployed_three_packet_order() {
    let humans = [solo_human()];
    let control = serialize_game_control(ServerGameControl::FinalStage, 0x1020_3040);
    let next = serialize_game_next_stage(4);
    let result = serialize_game_result(&GameResult {
        winning_team: None,
        humans: &humans,
        ais: &[],
    })
    .unwrap();

    let mut wrong_order = Ceremony::default();
    assert!(wrong_order.accept_next_stage(&next).is_err());
    assert_eq!(wrong_order.state(), CeremonyState::Racing);

    let mut ceremony = Ceremony::default();
    let decoded_control = ceremony.accept_game_control(&control).unwrap();
    assert_eq!(decoded_control.state, 4);
    assert_eq!(decoded_control.value0, 0x1020_3040);
    assert_eq!(decoded_control.encoded_status, 0);
    assert_eq!(ceremony.state(), CeremonyState::FinalStage);
    let decoded_next = ceremony.accept_next_stage(&next).unwrap();
    assert_eq!(decoded_next.game_type, 4);
    assert_eq!(decoded_next.stage_marker_1, 0);
    assert_eq!(decoded_next.stage_marker_2, 0);
    assert_eq!(ceremony.state(), CeremonyState::StageAdvanced);
    let decoded_result = ceremony.accept_game_result(&result).unwrap();
    assert_eq!(decoded_result.humans[0].player_id, 3);
    assert_eq!(ceremony.state(), CeremonyState::Podium);
}

fn solo_human() -> HumanRaceResult {
    HumanRaceResult {
        player_id: 3,
        finish_time: 123_456,
        kart_id: 1_777,
        rank: 0,
        current_rp: 700,
        earned_rp: 8,
        earned_lucci: 9,
        current_lucci: 1_234,
        team: None,
        team_points: 0,
        character_id: 55,
        club_mark_logo: 0,
    }
}

fn convert_to_old_csharp_record(packet: &[u8]) -> Vec<u8> {
    let record_start = 9;
    let record_end = record_start + 212;
    let record = &packet[record_start..record_end];
    let mut output = Vec::with_capacity(packet.len() + 5);
    output.extend_from_slice(&packet[..record_start]);
    output.extend_from_slice(&record[..63]);
    output.extend_from_slice(&record[64..68]);
    output.push(record[63]);
    output.extend_from_slice(&record[68..]);
    output.extend_from_slice(&[0; 5]);
    output.extend_from_slice(&packet[record_end..]);
    output
}

fn decode_hex(hex: &str) -> Vec<u8> {
    let hex = hex.trim();
    assert!(hex.len().is_multiple_of(2));
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(pair, 16).unwrap()
        })
        .collect()
}
