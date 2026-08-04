use p5136_core::{
    frame::{DEFAULT_MAX_PAYLOAD, FrameError, decode_encrypted, encode_encrypted},
    race_result_protocol::{
        EMPTY_GAME_RESULT_LENGTH, GameResult, HUMAN_RESULT_RECORD_LENGTH, HumanRaceResult,
        serialize_game_result,
    },
};

#[test]
fn four_to_five_humans_cross_1024_but_round_trip_under_the_real_frame_limit() {
    let four_player = serialize_human_result(4);
    let five_player = serialize_human_result(5);

    assert_eq!(
        four_player.len(),
        EMPTY_GAME_RESULT_LENGTH + 4 * HUMAN_RESULT_RECORD_LENGTH
    );
    assert_eq!(
        five_player.len(),
        EMPTY_GAME_RESULT_LENGTH + 5 * HUMAN_RESULT_RECORD_LENGTH
    );
    assert!(four_player.len() <= 1_024);
    assert!(five_player.len() > 1_024);
    assert_eq!(four_player.len(), 900);
    assert_eq!(five_player.len(), 1_112);

    let initial_iv = 0x5136_5136;
    let mut send_iv = initial_iv;
    let wire = encode_encrypted(&five_player, &mut send_iv, DEFAULT_MAX_PAYLOAD)
        .expect("the real 1 MiB transport limit must accept a five-player result");
    assert_eq!(wire.len(), five_player.len() + 8);

    let mut receive_iv = initial_iv;
    let decoded = decode_encrypted(&wire, &mut receive_iv, DEFAULT_MAX_PAYLOAD)
        .expect("the encrypted five-player result must decode");
    assert_eq!(decoded, five_player);
    assert_eq!(receive_iv, send_iv);

    let mut artificially_limited_iv = initial_iv;
    assert_eq!(
        encode_encrypted(&five_player, &mut artificially_limited_iv, 1_024),
        Err(FrameError::PayloadTooLarge {
            length: five_player.len(),
            maximum: 1_024,
        }),
        "a literal 1024-byte cap would reproduce a five-player boundary failure"
    );
}

fn serialize_human_result(count: usize) -> Vec<u8> {
    let humans = (0..count)
        .map(|index| HumanRaceResult {
            player_id: i32::try_from(index).unwrap(),
            finish_time: 100_000 + u32::try_from(index).unwrap(),
            kart_id: 1_400 + u16::try_from(index).unwrap(),
            rank: i32::try_from(index).unwrap(),
            current_rp: 20_000_000,
            earned_rp: 0,
            earned_lucci: 0,
            current_lucci: 0,
            team: None,
            team_points: 0,
            character_id: u16::try_from(index).unwrap(),
            club_mark_logo: 0,
        })
        .collect::<Vec<_>>();
    serialize_game_result(&GameResult {
        winning_team: None,
        humans: &humans,
        ais: &[],
    })
    .unwrap()
}
