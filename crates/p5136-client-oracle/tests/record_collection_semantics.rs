use p5136_client_oracle::{DecodeError, record_collection::decode_start_collect_record_reply};
use p5136_core::race_protocol::serialize_start_collect_record_reply;

#[test]
fn production_writer_matches_the_independent_native_consumer() {
    let disabled = decode_start_collect_record_reply(&serialize_start_collect_record_reply(false))
        .expect("canonical false response must decode");
    assert_eq!(disabled.stored_flag, 0);
    assert!(disabled.collector_gate_argument);
    assert!(!disabled.flag_is_nonzero());

    let enabled = decode_start_collect_record_reply(&serialize_start_collect_record_reply(true))
        .expect("canonical true response must decode");
    assert_eq!(enabled.stored_flag, 1);
    assert!(!enabled.collector_gate_argument);
    assert!(enabled.flag_is_nonzero());
}

#[test]
fn native_truthiness_is_preserved_without_weakening_the_writer() {
    let action = decode_start_collect_record_reply(&[0xF5, 0x07, 0xA4, 0x52, 0x7F]).unwrap();
    assert_eq!(action.stored_flag, 0x7F);
    assert!(!action.collector_gate_argument);
    assert!(action.flag_is_nonzero());
}

#[test]
fn exact_completion_boundary_rejects_wrong_hash_truncation_and_suffixes() {
    assert!(matches!(
        decode_start_collect_record_reply(&[0xF4, 0x07, 0xA4, 0x52, 1]),
        Err(DecodeError::UnexpectedHash { .. })
    ));
    let valid = [0xF5, 0x07, 0xA4, 0x52, 1];
    for length in 0..valid.len() {
        assert!(matches!(
            decode_start_collect_record_reply(&valid[..length]),
            Err(DecodeError::UnexpectedEof { .. })
        ));
    }
    assert!(matches!(
        decode_start_collect_record_reply(&[0xF5, 0x07, 0xA4, 0x52, 1, 0]),
        Err(DecodeError::TrailingBytes {
            offset: 5,
            remaining: 1,
        })
    ));
}
