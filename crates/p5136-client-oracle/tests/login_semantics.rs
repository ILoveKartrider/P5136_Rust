use std::net::Ipv4Addr;

use p5136_client_oracle::login::{decode_auth_reply, decode_channel_move_in, decode_login_reply};
use p5136_core::{
    channel::serialize_pr_channel_move_in,
    login::{
        AGREEMENT_URL, LEGACY_LOGIN_TOKEN, LegacyTime, PrLoginFields, serialize_pr_cn_authen_login,
        serialize_pr_login,
    },
};

#[test]
fn structural_login_readers_preserve_identity_time_and_udp_endpoints() {
    let auth_packet = serialize_pr_cn_authen_login().unwrap();
    let auth = decode_auth_reply(&auth_packet).unwrap();
    assert_eq!(auth.status, 1);
    assert_eq!(auth.token, LEGACY_LOGIN_TOKEN);
    assert_eq!(auth.agreement_url, AGREEMENT_URL);

    let fields = PrLoginFields {
        time: LegacyTime {
            days_since_1900: 0x1234,
            quarter_seconds: 0x5678,
        },
        user_no: 0x1020_3040,
        nickname: "테스트Rider".to_owned(),
        pmap: 0x5566_7788,
        advertised_address: Ipv4Addr::new(192, 168, 1, 10),
        game_udp_port: 39_311,
        p2p_udp_port: 39_312,
        screen: 7,
    };
    let login_packet = serialize_pr_login(&fields).unwrap();
    let login = decode_login_reply(&login_packet).unwrap();
    assert_eq!(login.status, 0);
    assert_eq!(login.days_since_1900, 0x1234);
    assert_eq!(login.quarter_seconds, 0x5678);
    assert_eq!(login.user_no, 0x1020_3040);
    assert_eq!(login.nickname, "테스트Rider");
    assert_eq!(login.pmap, 0x5566_7788);
    assert_eq!(login.game_udp.address, Ipv4Addr::new(192, 168, 1, 10));
    assert_eq!(login.game_udp.port, 39_311);
    assert_eq!(login.p2p_udp.port, 39_312);
    assert_eq!(login.content_label, "content");
    assert_eq!(login.country_key, "cc");
    assert_eq!(login.country_value, "kr");
    assert_eq!(login.screen, 7);

    let move_in_packet = serialize_pr_channel_move_in(41_111, 42_222);
    let move_in = decode_channel_move_in(&move_in_packet).unwrap();
    assert!(move_in.accepted);
    assert_eq!(move_in.game_udp.address, Ipv4Addr::UNSPECIFIED);
    assert_eq!(move_in.game_udp.port, 41_111);
    assert_eq!(move_in.p2p_udp.address, Ipv4Addr::UNSPECIFIED);
    assert_eq!(move_in.p2p_udp.port, 42_222);
}

#[test]
fn structural_login_reader_rejects_truncation_and_trailing_bytes() {
    let fields = PrLoginFields {
        time: LegacyTime {
            days_since_1900: 1,
            quarter_seconds: 2,
        },
        user_no: 3,
        nickname: "R".to_owned(),
        pmap: 4,
        advertised_address: Ipv4Addr::LOCALHOST,
        game_udp_port: 5,
        p2p_udp_port: 6,
        screen: 7,
    };
    let packet = serialize_pr_login(&fields).unwrap();
    for length in 0..packet.len() {
        assert!(decode_login_reply(&packet[..length]).is_err());
    }
    let mut extended = packet;
    extended.push(0);
    assert!(decode_login_reply(&extended).is_err());
}
