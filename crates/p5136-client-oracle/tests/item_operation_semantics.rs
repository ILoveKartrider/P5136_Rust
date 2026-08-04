use p5136_client_oracle::{
    item_client_fsm::{
        AUDITED_CONSUMER_BRANCH_COUNT, DeferredOutboundTransition, ItemClientFsm,
        ItemClientObjectKey, ItemClientTransitionOutcome, SUPPLEMENTAL_CONSUMER_BRANCH_COUNT,
        TOTAL_CONSUMER_BRANCH_COUNT,
    },
    item_operation::{Meaning, consume},
};
use p5136_core::{
    game_slot_item_semantics::ItemLifecycleMeaning,
    game_slot_protocol::{GAME_SLOT_PACKET_HASH, GameSlotBody, parse_game_slot_packet},
};
use p5136_server::{ItemOperationAuditDisposition, audit_game_slot_item_operation};

#[derive(Clone, Copy)]
struct Fixture {
    pair: (u32, u32),
    state: u32,
    length: usize,
    state_offset: usize,
    flag: Option<u8>,
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn raw(fixture: Fixture) -> Vec<u8> {
    let mut raw = vec![0_u8; fixture.length];
    put_u32(&mut raw, 0, fixture.pair.0);
    put_u32(&mut raw, 4, fixture.pair.1);
    put_u32(&mut raw, 8, 0x7000_0001);
    if fixture.pair == (0x3BEA_06CF, 0x5105_07EE) {
        raw[fixture.state_offset] = u8::try_from(fixture.state).unwrap();
    } else {
        put_u32(&mut raw, fixture.state_offset, fixture.state);
    }
    if let Some(flag) = fixture.flag {
        raw[28] = flag;
    }
    raw
}

fn server_wire(raw: &[u8]) -> Vec<u8> {
    let mut wire = vec![0_u8; 20 + raw.len()];
    put_u32(&mut wire, 0, GAME_SLOT_PACKET_HASH);
    put_u32(&mut wire, 8, 2);
    wire[12] = 12;
    put_u32(&mut wire, 16, u32::try_from(raw.len()).unwrap());
    wire[20..].copy_from_slice(raw);
    wire
}

fn parse_server(raw: &[u8]) -> p5136_core::game_slot_protocol::ItemOperation {
    let wire = server_wire(raw);
    let parsed = parse_game_slot_packet(&wire).unwrap();
    let GameSlotBody::ItemOperation(operation) = parsed.body() else {
        panic!("fixture did not reach the strict type-12 parser");
    };
    *operation
}

fn server_decodes_strict_item_operation(raw: &[u8]) -> bool {
    let wire = server_wire(raw);
    parse_game_slot_packet(&wire)
        .is_ok_and(|parsed| matches!(parsed.body(), GameSlotBody::ItemOperation(_)))
}

fn meaning(value: ItemLifecycleMeaning) -> Meaning {
    match value {
        ItemLifecycleMeaning::Unknown => Meaning::Unknown,
        ItemLifecycleMeaning::Place => Meaning::Place,
        ItemLifecycleMeaning::Launch => Meaning::Launch,
        ItemLifecycleMeaning::Activate => Meaning::Activate,
        ItemLifecycleMeaning::Impact => Meaning::Impact,
        ItemLifecycleMeaning::Resolve => Meaning::Resolve,
        ItemLifecycleMeaning::Retarget => Meaning::Retarget,
        ItemLifecycleMeaning::Remove => Meaning::Remove,
        ItemLifecycleMeaning::UpdateRuntimeFlag => Meaning::UpdateRuntimeFlag,
        ItemLifecycleMeaning::NoClientAction => Meaning::NoClientAction,
        _ => panic!("this oracle slice does not model {value:?}"),
    }
}

fn assert_same(raw: &[u8]) {
    let client = consume(raw).unwrap();
    let server = parse_server(raw);
    assert_eq!(server.schema.class_name, client.class_name);
    assert_eq!(server.object_id, client.object_id);
    assert_eq!(server.state, client.state);
    assert_eq!(meaning(server.semantics.meaning), client.meaning);
    assert_eq!(server.semantics.native_phase, client.native_phase);
    assert_eq!(server.semantics.transition_token, client.transition_token);
    assert_eq!(server.semantics.source_object_id, client.source_object_id);
    assert_eq!(server.semantics.target_object_id, client.target_object_id);
    assert_eq!(
        server
            .semantics
            .target_object_ids
            .map(|list| list.decode(raw).unwrap())
            .unwrap_or_default(),
        client.target_object_ids
    );
    assert_eq!(server.semantics.variant, client.variant);
    assert_eq!(server.semantics.effect_item_id, client.effect_item_id);
}

#[allow(
    clippy::too_many_lines,
    reason = "the independent table keeps every modeled native client branch explicit"
)]
fn all_recovered_fixtures() -> Vec<Fixture> {
    let mut fixtures = Vec::new();
    for (state, length) in [(0, 25), (2, 28)] {
        fixtures.push(Fixture {
            pair: (0x0D49_030D, 0x184D_042C),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
    }
    for (state, length) in [(0, 28), (2, 34)] {
        fixtures.push(Fixture {
            pair: (0x2271_0505, 0x3310_0624),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
    }
    for (pair, state, length, state_offset, flag) in [
        ((0x233A_0538, 0x33D9_0657), 1, 77, 12, None),
        ((0x233A_0538, 0x33D9_0657), 2, 64, 12, None),
        ((0x233A_0538, 0x33D9_0657), 3, 68, 12, None),
        ((0x233A_0538, 0x33D9_0657), 4, 16, 12, None),
        ((0x1DB9_04A4, 0x2D39_05C3), 1, 63, 16, None),
        ((0x1DB9_04A4, 0x2D39_05C3), 2, 33, 16, Some(1)),
        ((0x1DB9_04A4, 0x2D39_05C3), 3, 33, 16, Some(1)),
        ((0x14A7_03E3, 0x21E9_0502), 1, 91, 16, None),
        ((0x14A7_03E3, 0x21E9_0502), 2, 33, 16, Some(1)),
        ((0x14A7_03E3, 0x21E9_0502), 3, 33, 16, Some(1)),
        ((0x2369_052B, 0x3408_064A), 1, 28, 12, None),
        ((0x2369_052B, 0x3408_064A), 2, 28, 12, None),
        ((0x2369_052B, 0x3408_064A), 3, 28, 12, None),
        ((0x2369_052B, 0x3408_064A), 4, 28, 12, None),
    ] {
        fixtures.push(Fixture {
            pair,
            state,
            length,
            state_offset,
            flag,
        });
    }
    for (pair, state, length) in [
        ((0x14A6_03ED, 0x21E8_050C), 1, 28),
        ((0x14A6_03ED, 0x21E8_050C), 2, 16),
        ((0x0D59_0311, 0x185D_0430), 1, 89),
        ((0x0D59_0311, 0x185D_0430), 2, 29),
        ((0x1DC1_04AE, 0x2D41_05CD), 1, 137),
        ((0x1DC1_04AE, 0x2D41_05CD), 2, 29),
        ((0x1DC1_04AE, 0x2D41_05CD), 3, 29),
        ((0x0A4F_02A5, 0x1434_03C4), 1, 24),
        ((0x0A4F_02A5, 0x1434_03C4), 2, 28),
        ((0x273E_0563, 0x38FC_0682), 0, 69),
        ((0x273E_0563, 0x38FC_0682), 1, 77),
        ((0x17FB_040D, 0x265C_052C), 1, 21),
        ((0x17FB_040D, 0x265C_052C), 2, 16),
        ((0x1977_0461, 0x27D8_0580), 1, 30),
        ((0x1977_0461, 0x27D8_0580), 2, 29),
        ((0x10D3_0380, 0x1CF6_049F), 1, 30),
        ((0x10D3_0380, 0x1CF6_049F), 2, 29),
        ((0x0DC1_0333, 0x18C5_0452), 1, 72),
        ((0x0DC1_0333, 0x18C5_0452), 2, 28),
        ((0x1DC5_04A1, 0x2D45_05C0), 1, 72),
        ((0x1DC5_04A1, 0x2D45_05C0), 2, 24),
        ((0x1E29_04C1, 0x2DA9_05E0), 1, 72),
        ((0x1E29_04C1, 0x2DA9_05E0), 2, 24),
        ((0x3442_0652, 0x483E_0771), 3, 28),
        ((0x42B8_070F, 0x58F2_082E), 4, 28),
        ((0x2856_057F, 0x3A14_069E), 0x7111_0001, 20),
        ((0x22FA_051F, 0x1D74_04AF), 2, 20),
        ((0x07AE_0248, 0x1074_0367), 0, 26),
        ((0x0D8B_032B, 0x188F_044A), 1, 29),
        ((0x10C3_0382, 0x1CE6_04A1), 1, 78),
        ((0x1942_0457, 0x27A3_0576), 1, 30),
        ((0x196B_0451, 0x27CC_0570), 1, 29),
        ((0x196B_0451, 0x27CC_0570), 2, 29),
        ((0x2E54_05E8, 0x4131_0707), 0, 26),
        ((0x3C6F_06D4, 0x518A_07F3), 1, 58),
        ((0x3C6F_06D4, 0x518A_07F3), 2, 24),
        ((0x3C6F_06D4, 0x518A_07F3), 3, 24),
    ] {
        fixtures.push(Fixture {
            pair,
            state,
            length,
            state_offset: if matches!(pair.0, 0x0D59_0311 | 0x1DC1_04AE) {
                16
            } else {
                12
            },
            flag: None,
        });
    }
    for (state, length) in [(0, 30), (2, 29), (3, 29), (4, 29), (5, 29), (7, 17)] {
        fixtures.push(Fixture {
            pair: (0x2262_0502, 0x3301_0621),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
    }
    for pair in [(0x0D7B_031D, 0x187F_043C), (0x10CA_034F, 0x1CED_046E)] {
        for (state, length) in [(1, 73), (2, 20)] {
            fixtures.push(Fixture {
                pair,
                state,
                length,
                state_offset: 12,
                flag: None,
            });
        }
    }
    fixtures.push(Fixture {
        pair: (0x10DE_0382, 0x1D01_04A1),
        state: 1,
        length: 30,
        state_offset: 12,
        flag: None,
    });
    for (state, length) in [(1, 24), (2, 20)] {
        fixtures.push(Fixture {
            pair: (0x1DB2_04AF, 0x2D32_05CE),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
    }
    for (pair, length) in [
        ((0x0D69_031A, 0x186D_0439), 31),
        ((0x1476_03D8, 0x21B8_04F7), 31),
        ((0x18D8_0444, 0x2739_0563), 27),
    ] {
        fixtures.push(Fixture {
            pair,
            state: 1,
            length,
            state_offset: 12,
            flag: None,
        });
    }
    for pair in [(0x1900_0448, 0x2761_0567), (0x19EB_046D, 0x284C_058C)] {
        fixtures.extend([
            Fixture {
                pair,
                state: 1,
                length: 120,
                state_offset: 12,
                flag: None,
            },
            Fixture {
                pair,
                state: 2,
                length: 28,
                state_offset: 12,
                flag: None,
            },
            Fixture {
                pair,
                state: 3,
                length: 28,
                state_offset: 12,
                flag: None,
            },
            Fixture {
                pair,
                state: 4,
                length: 28,
                state_offset: 12,
                flag: None,
            },
        ]);
    }
    fixtures.extend([
        Fixture {
            pair: (0x2DC1_05C8, 0x409E_06E7),
            state: 1,
            length: 121,
            state_offset: 12,
            flag: None,
        },
        Fixture {
            pair: (0x2DC1_05C8, 0x409E_06E7),
            state: 2,
            length: 33,
            state_offset: 12,
            flag: None,
        },
        Fixture {
            pair: (0x2DC1_05C8, 0x409E_06E7),
            state: 3,
            length: 33,
            state_offset: 12,
            flag: None,
        },
    ]);
    for pair in [(0x42E4_071F, 0x591E_083E), (0x2954_059D, 0x3B12_06BC)] {
        fixtures.extend([
            Fixture {
                pair,
                state: 1,
                length: 132,
                state_offset: 12,
                flag: None,
            },
            Fixture {
                pair,
                state: 2,
                length: 28,
                state_offset: 12,
                flag: None,
            },
            Fixture {
                pair,
                state: 3,
                length: 24,
                state_offset: 12,
                flag: None,
            },
            Fixture {
                pair,
                state: 4,
                length: 24,
                state_offset: 12,
                flag: None,
            },
        ]);
    }
    fixtures.extend([
        Fixture {
            pair: (0x6381_08BF, 0x7E37_09DE),
            state: 1,
            length: 132,
            state_offset: 12,
            flag: None,
        },
        Fixture {
            pair: (0x6381_08BF, 0x7E37_09DE),
            state: 2,
            length: 32,
            state_offset: 12,
            flag: None,
        },
        Fixture {
            pair: (0x6381_08BF, 0x7E37_09DE),
            state: 3,
            length: 28,
            state_offset: 12,
            flag: None,
        },
    ]);
    for (state, length) in [(1, 73), (2, 29), (3, 29), (4, 29), (7, 29)] {
        fixtures.push(Fixture {
            pair: (0x1E04_04B2, 0x2D84_05D1),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
    }
    fixtures.extend([
        Fixture {
            pair: (0x1909_043E, 0x276A_055D),
            state: 1,
            length: 85,
            state_offset: 16,
            flag: None,
        },
        Fixture {
            pair: (0x1909_043E, 0x276A_055D),
            state: 2,
            length: 33,
            state_offset: 16,
            flag: Some(0),
        },
        Fixture {
            pair: (0x1909_043E, 0x276A_055D),
            state: 2,
            length: 33,
            state_offset: 16,
            flag: Some(1),
        },
        Fixture {
            pair: (0x1909_043E, 0x276A_055D),
            state: 3,
            length: 33,
            state_offset: 16,
            flag: Some(1),
        },
        Fixture {
            pair: (0x1909_043E, 0x276A_055D),
            state: 4,
            length: 33,
            state_offset: 16,
            flag: Some(1),
        },
        Fixture {
            pair: (0x1909_043E, 0x276A_055D),
            state: 5,
            length: 24,
            state_offset: 16,
            flag: None,
        },
    ]);
    for pair in [(0x41CC_070F, 0x3996_067F), (0x2DDA_05D7, 0x40B7_06F6)] {
        for (state, length) in [(1, 24), (2, 28), (3, 24), (4, 24)] {
            fixtures.push(Fixture {
                pair,
                state,
                length,
                state_offset: 12,
                flag: None,
            });
        }
    }
    for (state, length) in [(1, 24), (2, 32), (3, 28)] {
        fixtures.push(Fixture {
            pair: (0x48D7_0757, 0x6030_0876),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
    }
    for pair in [(0x2EC5_05FC, 0x41A2_071B), (0x196A_0455, 0x27CB_0574)] {
        for (state, length) in [(1, 24), (2, 28), (3, 28), (4, 28)] {
            fixtures.push(Fixture {
                pair,
                state,
                length,
                state_offset: 12,
                flag: None,
            });
        }
    }
    fixtures.extend([
        Fixture {
            pair: (0x276B_0567, 0x3929_0686),
            state: 0,
            length: 29,
            state_offset: 13,
            flag: None,
        },
        Fixture {
            pair: (0x276B_0567, 0x3929_0686),
            state: 4,
            length: 29,
            state_offset: 13,
            flag: None,
        },
    ]);

    for (pair, cases) in [
        ((0x1457_03C9, 0x2199_04E8), &[(1, 33), (2, 24)][..]),
        ((0x1E52_04C0, 0x2DD2_05DF), &[(1, 72), (2, 24)][..]),
        ((0x1110_037F, 0x1D33_049E), &[(1, 31), (2, 29)][..]),
        ((0x3473_0640, 0x486F_075F), &[(0, 27), (2, 25), (3, 25)][..]),
        ((0x07CF_0250, 0x1095_036F), &[(1, 33), (2, 20)][..]),
    ] {
        for &(state, length) in cases {
            fixtures.push(Fixture {
                pair,
                state,
                length,
                state_offset: 12,
                flag: None,
            });
        }
    }
    for (state, length) in [
        (1, 20),
        (2, 17),
        (3, 13),
        (4, 25),
        (5, 18),
        (6, 18),
        (7, 26),
        (8, 26),
        (9, 25),
    ] {
        fixtures.push(Fixture {
            pair: (0x3BEA_06CF, 0x5105_07EE),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
    }
    for (state, length) in [(1, 30), (2, 25), (3, 29)] {
        fixtures.push(Fixture {
            pair: (0x2973_05B1, 0x3B31_06D0),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
    }

    for (state, length) in [(1, 72), (2, 29), (3, 29), (5, 25)] {
        fixtures.push(Fixture {
            pair: (0x1DC6_04B1, 0x2D46_05D0),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
    }
    for (state, length) in [(1, 73), (2, 29), (3, 25)] {
        fixtures.push(Fixture {
            pair: (0x07C0_024A, 0x1086_0369),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
    }
    for state in [1, 2] {
        fixtures.push(Fixture {
            pair: (0x150D_03E9, 0x224F_0508),
            state,
            length: 29,
            state_offset: 12,
            flag: None,
        });
    }
    for (state, length) in [(1, 26), (2, 31)] {
        fixtures.push(Fixture {
            pair: (0x0DB2_0327, 0x18B6_0446),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
    }
    for (state, length) in [(0, 25), (1, 24), (2, 25)] {
        fixtures.push(Fixture {
            pair: (0x28A5_0580, 0x3A63_069F),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
    }
    for (state, length) in [(0, 30), (1, 29), (2, 17)] {
        fixtures.push(Fixture {
            pair: (0x2E3D_05E0, 0x411A_06FF),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
    }

    assert_eq!(fixtures.len(), 165);
    fixtures
}

fn is_supplemental_consumer(fixture: Fixture) -> bool {
    matches!(
        fixture.pair,
        (0x233A_0538, 0x33D9_0657)
            | (0x1DB9_04A4, 0x2D39_05C3)
            | (0x14A7_03E3, 0x21E9_0502)
            | (0x2369_052B, 0x3408_064A)
            | (0x2271_0505, 0x3310_0624)
    )
}

#[test]
fn item_client_fsm_executes_the_original_149_consumer_branches() {
    let fixtures = all_recovered_fixtures()
        .into_iter()
        .filter(|fixture| !is_supplemental_consumer(*fixture))
        .collect::<Vec<_>>();
    assert_eq!(fixtures.len(), AUDITED_CONSUMER_BRANCH_COUNT);

    let mut fsm = ItemClientFsm::new();
    let mut local = 0;
    let mut deferred = 0;
    let mut unknown = 0;
    for fixture in fixtures {
        match fsm.accept(&raw(fixture)).unwrap().outcome {
            ItemClientTransitionOutcome::LocalOnly => local += 1,
            ItemClientTransitionOutcome::DeferredOutbound => deferred += 1,
            ItemClientTransitionOutcome::UnknownSideEffect => unknown += 1,
            ItemClientTransitionOutcome::ImmediateOutbound => {
                panic!("no recovered consumer branch sends synchronously")
            }
        }
    }

    assert_eq!(
        fsm.accepted_transition_count(),
        AUDITED_CONSUMER_BRANCH_COUNT
    );
    assert_eq!(
        (local, deferred, unknown),
        (71, 69, 9),
        "the reviewed 149-branch side-effect census changed"
    );
}

#[test]
fn rust_server_admits_and_byte_preserves_the_original_149_consumer_branches() {
    let fixtures = all_recovered_fixtures()
        .into_iter()
        .filter(|fixture| !is_supplemental_consumer(*fixture))
        .collect::<Vec<_>>();
    assert_eq!(fixtures.len(), AUDITED_CONSUMER_BRANCH_COUNT);

    let mut tracked = 0;
    let mut untracked = 0;
    let mut suppressed = 0;
    for fixture in fixtures {
        let body = raw(fixture);
        let wire = server_wire(&body);
        let audit = audit_game_slot_item_operation(&wire, 0, 2).unwrap_or_else(|error| {
            panic!(
                "server audit rejected pair 0x{:08X}/0x{:08X} state {}: {error}",
                fixture.pair.0, fixture.pair.1, fixture.state
            )
        });
        assert_eq!(
            audit.relay_bytes, wire,
            "server relay must remain byte-exact"
        );
        assert_eq!(audit.state, fixture.state);
        match audit.disposition {
            ItemOperationAuditDisposition::PublishTracked => tracked += 1,
            ItemOperationAuditDisposition::PublishUntracked => untracked += 1,
            ItemOperationAuditDisposition::SuppressDuplicate => suppressed += 1,
        }
    }

    assert_eq!(
        (tracked, untracked, suppressed),
        (87, 62, 0),
        "the reviewed 149-branch server admission census changed"
    );
}

#[test]
fn supplemental_controller_and_course_consumers_share_the_executable_fsm() {
    let mut fsm = ItemClientFsm::new();
    let supplemental = all_recovered_fixtures()
        .into_iter()
        .filter(|fixture| is_supplemental_consumer(*fixture))
        .collect::<Vec<_>>();
    assert_eq!(supplemental.len() + 1, SUPPLEMENTAL_CONSUMER_BRANCH_COUNT);
    for fixture in supplemental {
        fsm.accept(&raw(fixture)).unwrap();
    }

    let mut course = vec![0_u8; 32];
    put_u32(&mut course, 0, 0x1139_0397);
    put_u32(&mut course, 4, 0x0D73_0327);
    put_u32(&mut course, 8, 0x7000_0001);
    put_u32(&mut course, 12, 0x7200_0002);
    put_u32(&mut course, 16, 4);
    for (index, unit) in "goal".encode_utf16().enumerate() {
        course[20 + index * 2..22 + index * 2].copy_from_slice(&unit.to_le_bytes());
    }
    put_u32(&mut course, 28, 0x7300_0003);
    let transition = fsm.accept(&course).unwrap();
    assert_eq!(transition.operation.meaning, Meaning::NoClientAction);
    assert_eq!(transition.outcome, ItemClientTransitionOutcome::LocalOnly);
    assert_eq!(
        fsm.accepted_transition_count(),
        SUPPLEMENTAL_CONSUMER_BRANCH_COUNT
    );
    assert_eq!(
        AUDITED_CONSUMER_BRANCH_COUNT + SUPPLEMENTAL_CONSUMER_BRANCH_COUNT,
        TOTAL_CONSUMER_BRANCH_COUNT
    );
}

#[test]
fn item_client_fsm_tracks_lifecycle_without_inventing_unknown_or_noop_state() {
    let mut fsm = ItemClientFsm::new();
    let cloud = Fixture {
        pair: (0x0D7B_031D, 0x187F_043C),
        state: 1,
        length: 73,
        state_offset: 12,
        flag: None,
    };
    let activated = fsm.accept(&raw(cloud)).unwrap();
    assert_eq!(
        activated.outcome,
        ItemClientTransitionOutcome::DeferredOutbound
    );
    assert!(fsm.object("GopCloud", 0x7000_0001).is_some());
    assert_eq!(fsm.pending_deferred_outbound(), 1);
    assert_eq!(
        fsm.take_deferred_outbound(),
        [DeferredOutboundTransition {
            key: ItemClientObjectKey {
                class_name: "GopCloud",
                object_id: 0x7000_0001,
            },
            consumed_state: 1,
            consumed_meaning: Meaning::Place,
        }]
    );
    assert_eq!(fsm.pending_deferred_outbound(), 0);
    fsm.accept(&raw(cloud)).unwrap();
    assert_eq!(fsm.pending_deferred_outbound(), 1);

    let resolved = fsm
        .accept(&raw(Fixture {
            state: 2,
            length: 20,
            ..cloud
        }))
        .unwrap();
    assert_eq!(resolved.outcome, ItemClientTransitionOutcome::LocalOnly);
    assert_eq!(
        fsm.object("GopCloud", 0x7000_0001).map(|state| state.state),
        Some(2)
    );
    assert_eq!(fsm.pending_deferred_outbound(), 0);

    let speed_down = Fixture {
        pair: (0x1DB2_04AF, 0x2D32_05CE),
        state: 1,
        length: 24,
        state_offset: 12,
        flag: None,
    };
    assert_eq!(
        fsm.accept(&raw(speed_down)).unwrap().outcome,
        ItemClientTransitionOutcome::LocalOnly,
        "SpeedDown has no recovered producer continuation"
    );
    let removed = fsm
        .accept(&raw(Fixture {
            state: 2,
            length: 20,
            ..speed_down
        }))
        .unwrap();
    assert_eq!(removed.outcome, ItemClientTransitionOutcome::LocalOnly);
    assert!(fsm.object("GopSpeedDown", 0x7000_0001).is_none());

    let silence = Fixture {
        pair: (0x150D_03E9, 0x224F_0508),
        state: 1,
        length: 29,
        state_offset: 12,
        flag: None,
    };
    fsm.accept(&raw(silence)).unwrap();
    let before = fsm.object("GopSilence", 0x7000_0001).cloned();
    let no_action = fsm
        .accept(&raw(Fixture {
            state: 2,
            ..silence
        }))
        .unwrap();
    assert_eq!(no_action.operation.meaning, Meaning::NoClientAction);
    assert_eq!(fsm.object("GopSilence", 0x7000_0001).cloned(), before);
}

#[test]
fn item_client_fsm_keeps_timed_angel_after_repeatable_defense_impact() {
    let mut fsm = ItemClientFsm::new();
    let angel_activation = fsm
        .accept(&raw(Fixture {
            pair: (0x0D49_030D, 0x184D_042C),
            state: 0,
            length: 25,
            state_offset: 12,
            flag: None,
        }))
        .unwrap();
    assert_eq!(
        angel_activation.outcome,
        ItemClientTransitionOutcome::DeferredOutbound
    );
    assert!(fsm.object("GopAngel", 0x7000_0001).is_some());
    assert_eq!(fsm.pending_deferred_outbound(), 1);

    let mut first_impact_packet = raw(Fixture {
        pair: (0x0D49_030D, 0x184D_042C),
        state: 2,
        length: 28,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut first_impact_packet, 8, 0x7000_0002);
    let first_defense_impact = fsm.accept(&first_impact_packet).unwrap();
    assert_eq!(
        first_defense_impact.outcome,
        ItemClientTransitionOutcome::LocalOnly
    );
    assert_eq!(first_defense_impact.operation.meaning, Meaning::Impact);
    assert!(fsm.object("GopAngel", 0x7000_0001).is_some());
    assert!(fsm.object("GopAngel", 0x7000_0002).is_some());
    assert_eq!(fsm.pending_deferred_outbound(), 1);

    let mut second_impact_packet = first_impact_packet;
    put_u32(&mut second_impact_packet, 8, 0x7000_0003);
    let second_defense_impact = fsm.accept(&second_impact_packet).unwrap();
    assert_eq!(second_defense_impact.operation.meaning, Meaning::Impact);
    assert!(second_defense_impact.previous.is_none());
    assert!(second_defense_impact.current.is_some());
    assert!(fsm.object("GopAngel", 0x7000_0001).is_some());
    assert_eq!(fsm.pending_deferred_outbound(), 1);
}

#[test]
fn gold_shield_codec_selects_all_three_defense_items_and_keeps_activation_armed() {
    let mut fsm = ItemClientFsm::new();
    let mut activation = raw(Fixture {
        pair: (0x2271_0505, 0x3310_0624),
        state: 0,
        length: 28,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut activation, 16, 0x7100_0001);
    put_u32(&mut activation, 20, 0x7200_0002);
    put_u32(&mut activation, 24, 0);
    let gold = fsm.accept(&activation).unwrap();
    assert_eq!(gold.operation.meaning, Meaning::Activate);
    assert_eq!(gold.operation.effect_item_id, Some(36));
    assert_eq!(gold.outcome, ItemClientTransitionOutcome::DeferredOutbound);
    assert_eq!(
        audit_game_slot_item_operation(&server_wire(&activation), 0, 2)
            .unwrap()
            .disposition,
        ItemOperationAuditDisposition::PublishTracked
    );

    let mut protect = activation.clone();
    put_u32(&mut protect, 8, 0x7000_0002);
    put_u32(&mut protect, 24, 3);
    assert_eq!(consume(&protect).unwrap().effect_item_id, Some(81));
    assert_same(&protect);

    let mut impact = raw(Fixture {
        pair: (0x2271_0505, 0x3310_0624),
        state: 2,
        length: 34,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut impact, 8, 0x7000_0003);
    put_u32(&mut impact, 16, 0x7100_0004);
    put_u32(&mut impact, 20, 0x7200_0005);
    put_u32(&mut impact, 24, 0x7300_0006);
    put_u32(&mut impact, 28, 0);
    assert_eq!(consume(&impact).unwrap().effect_item_id, Some(36));
    assert_same(&impact);

    let mut protect_impact = impact.clone();
    put_u32(&mut protect_impact, 28, 3);
    assert_eq!(consume(&protect_impact).unwrap().effect_item_id, Some(81));
    assert_same(&protect_impact);

    impact[32..34].copy_from_slice(&106_u16.to_le_bytes());
    let siren = fsm.accept(&impact).unwrap();
    assert_eq!(siren.operation.meaning, Meaning::Impact);
    assert_eq!(siren.operation.effect_item_id, Some(106));
    assert_eq!(siren.outcome, ItemClientTransitionOutcome::LocalOnly);
    assert_same(&impact);
    assert_eq!(
        audit_game_slot_item_operation(&server_wire(&impact), 0, 2)
            .unwrap()
            .disposition,
        ItemOperationAuditDisposition::PublishTracked
    );
    assert!(fsm.object("GopGoldShield", 0x7000_0001).is_some());
    assert_eq!(fsm.pending_deferred_outbound(), 1);

    let mut invalid_kind = activation;
    put_u32(&mut invalid_kind, 24, 2);
    assert!(consume(&invalid_kind).is_err());
    assert_eq!(
        parse_server(&invalid_kind).semantics.meaning,
        ItemLifecycleMeaning::Unknown,
        "invalid native kinds must relay without authoritative mutation"
    );
    assert_eq!(
        audit_game_slot_item_operation(&server_wire(&invalid_kind), 0, 2)
            .unwrap()
            .disposition,
        ItemOperationAuditDisposition::PublishUntracked
    );

    let native_default = raw(Fixture {
        pair: (0x2271_0505, 0x3310_0624),
        state: 1,
        length: 16,
        state_offset: 12,
        flag: None,
    });
    assert!(consume(&native_default).is_err());
    assert_eq!(
        parse_server(&native_default).semantics.meaning,
        ItemLifecycleMeaning::Unknown,
        "the native default writer shape is exact but has no recovered consumer meaning"
    );
    assert_eq!(
        audit_game_slot_item_operation(&server_wire(&native_default), 0, 2)
            .unwrap()
            .disposition,
        ItemOperationAuditDisposition::PublishUntracked
    );
}

#[test]
fn unknown_and_noop_branches_do_not_cancel_a_proven_deferred_marker() {
    let mut conservative = ItemClientFsm::new();
    let straight_rocket = Fixture {
        pair: (0x3C6F_06D4, 0x518A_07F3),
        state: 1,
        length: 58,
        state_offset: 12,
        flag: None,
    };
    conservative.accept(&raw(straight_rocket)).unwrap();
    let unknown_follow_up = conservative
        .accept(&raw(Fixture {
            state: 2,
            length: 24,
            ..straight_rocket
        }))
        .unwrap();
    assert_eq!(
        unknown_follow_up.outcome,
        ItemClientTransitionOutcome::UnknownSideEffect
    );
    assert_eq!(conservative.pending_deferred_outbound(), 1);

    conservative.reset_race();
    let water_mine = Fixture {
        pair: (0x1E04_04B2, 0x2D84_05D1),
        state: 1,
        length: 73,
        state_offset: 12,
        flag: None,
    };
    conservative.accept(&raw(water_mine)).unwrap();
    let no_action_follow_up = conservative
        .accept(&raw(Fixture {
            state: 7,
            length: 29,
            ..water_mine
        }))
        .unwrap();
    assert_eq!(
        no_action_follow_up.operation.meaning,
        Meaning::NoClientAction
    );
    assert_eq!(conservative.pending_deferred_outbound(), 1);
}

#[test]
fn item_client_fsm_rejects_malformed_input_transactionally() {
    let mut fsm = ItemClientFsm::new();
    let valid = raw(Fixture {
        pair: (0x0D7B_031D, 0x187F_043C),
        state: 1,
        length: 73,
        state_offset: 12,
        flag: None,
    });
    fsm.accept(&valid).unwrap();
    let object_count = fsm.active_object_count();
    let accepted = fsm.accepted_transition_count();
    let deferred = fsm.pending_deferred_outbound();

    assert!(fsm.accept(&valid[..72]).is_err());
    assert_eq!(fsm.active_object_count(), object_count);
    assert_eq!(fsm.accepted_transition_count(), accepted);
    assert_eq!(fsm.pending_deferred_outbound(), deferred);
}

#[test]
fn cloud_magnet_and_speed_down_keep_class_specific_actor_roles() {
    const TOKEN: u32 = 0x7100_0001;
    const SOURCE: u32 = 0x7200_0002;
    const TARGET: u32 = 0x7300_0003;

    for pair in [(0x0D7B_031D, 0x187F_043C), (0x10CA_034F, 0x1CED_046E)] {
        let mut install = raw(Fixture {
            pair,
            state: 1,
            length: 73,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut install, 16, TOKEN);
        put_u32(&mut install, 20, SOURCE);
        install[24] = 7;
        let consumed = consume(&install).unwrap();
        assert_eq!(consumed.meaning, Meaning::Place);
        assert_eq!(consumed.native_phase, Some(0));
        assert_eq!(consumed.transition_token, Some(TOKEN));
        assert_eq!(consumed.source_object_id, Some(SOURCE));
        assert_eq!(consumed.target_object_id, None);
        assert_eq!(consumed.variant, Some(7));
        assert_same(&install);

        let mut impact = raw(Fixture {
            pair,
            state: 2,
            length: 20,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut impact, 16, TARGET);
        let consumed = consume(&impact).unwrap();
        assert_eq!(consumed.meaning, Meaning::Impact);
        assert_eq!(consumed.native_phase, Some(2));
        assert_eq!(consumed.transition_token, None);
        assert_eq!(consumed.source_object_id, None);
        assert_eq!(consumed.target_object_id, Some(TARGET));
        assert_same(&impact);
    }

    let mut magnet = raw(Fixture {
        pair: (0x10DE_0382, 0x1D01_04A1),
        state: 1,
        length: 30,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut magnet, 16, TOKEN);
    put_u32(&mut magnet, 20, SOURCE);
    put_u32(&mut magnet, 24, TARGET);
    magnet[28..30].copy_from_slice(&0x3344_u16.to_le_bytes());
    let consumed = consume(&magnet).unwrap();
    assert_eq!(consumed.meaning, Meaning::Activate);
    assert_eq!(consumed.native_phase, Some(1));
    assert_eq!(consumed.transition_token, Some(TOKEN));
    assert_eq!(consumed.source_object_id, Some(SOURCE));
    assert_eq!(consumed.target_object_id, Some(TARGET));
    assert_eq!(consumed.variant, None);
    assert_same(&magnet);

    let mut speed_down = raw(Fixture {
        pair: (0x1DB2_04AF, 0x2D32_05CE),
        state: 1,
        length: 24,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut speed_down, 16, TOKEN);
    put_u32(&mut speed_down, 20, TARGET);
    let consumed = consume(&speed_down).unwrap();
    assert_eq!(consumed.meaning, Meaning::Activate);
    assert_eq!(consumed.native_phase, Some(0));
    assert_eq!(consumed.transition_token, Some(TOKEN));
    assert_eq!(consumed.source_object_id, None);
    assert_eq!(consumed.target_object_id, Some(TARGET));
    assert_same(&speed_down);

    let mut remove = raw(Fixture {
        pair: (0x1DB2_04AF, 0x2D32_05CE),
        state: 2,
        length: 20,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut remove, 16, TOKEN);
    let consumed = consume(&remove).unwrap();
    assert_eq!(consumed.meaning, Meaning::Remove);
    assert_eq!(consumed.native_phase, Some(2));
    assert_eq!(consumed.transition_token, Some(TOKEN));
    assert_eq!(consumed.target_object_id, None);
    assert_same(&remove);
}

#[test]
fn devil_variants_bind_only_the_client_selected_target() {
    const TOKEN: u32 = 0x7400_0001;
    const SOURCE: u32 = 0x7500_0002;
    const TARGET: u32 = 0x7600_0003;

    for pair in [(0x0D69_031A, 0x186D_0439), (0x1476_03D8, 0x21B8_04F7)] {
        let mut targeted = raw(Fixture {
            pair,
            state: 1,
            length: 31,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut targeted, 16, TOKEN);
        targeted[20] = 5;
        put_u32(&mut targeted, 21, SOURCE);
        targeted[25] = 7;
        targeted[26] = 8;
        put_u32(&mut targeted, 27, TARGET);
        let consumed = consume(&targeted).unwrap();
        assert_eq!(consumed.meaning, Meaning::Activate);
        assert_eq!(consumed.native_phase, Some(0));
        assert_eq!(consumed.transition_token, Some(TOKEN));
        assert_eq!(consumed.source_object_id, Some(SOURCE));
        assert_eq!(consumed.target_object_id, Some(TARGET));
        assert_eq!(consumed.variant, Some(5));
        assert_same(&targeted);

        targeted[20] = 4;
        let consumed = consume(&targeted).unwrap();
        assert_eq!(consumed.target_object_id, None);
        assert_eq!(consumed.variant, Some(4));
        assert_same(&targeted);
    }

    let mut new_devil = raw(Fixture {
        pair: (0x18D8_0444, 0x2739_0563),
        state: 1,
        length: 27,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut new_devil, 16, TOKEN);
    new_devil[20] = 5;
    put_u32(&mut new_devil, 21, SOURCE);
    new_devil[25] = 7;
    new_devil[26] = 8;
    let consumed = consume(&new_devil).unwrap();
    assert_eq!(consumed.source_object_id, Some(SOURCE));
    assert_eq!(consumed.target_object_id, None);
    assert_eq!(consumed.variant, Some(5));
    assert_same(&new_devil);
}

#[test]
fn all_newly_recovered_client_state_branches_match_the_server_decoder() {
    for fixture in all_recovered_fixtures() {
        assert_same(&raw(fixture));
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "distinct nonzero fields keep the independent fourth-pass offset table auditable"
)]
fn fourth_pass_oracle_distinguishes_every_recovered_actor_and_token_offset() {
    const TOKEN: u32 = 0x7100_0001;
    const SOURCE: u32 = 0x7200_0002;
    const TARGET: u32 = 0x7300_0003;

    struct Case {
        fixture: Fixture,
        token_offset: usize,
        source_offset: usize,
        target_offset: Option<usize>,
        variant_offset: usize,
        meaning: Meaning,
        phase: Option<u8>,
    }

    let cases = [
        Case {
            fixture: Fixture {
                pair: (0x0D49_030D, 0x184D_042C),
                state: 0,
                length: 25,
                state_offset: 12,
                flag: None,
            },
            token_offset: 16,
            source_offset: 21,
            target_offset: None,
            variant_offset: 20,
            meaning: Meaning::Activate,
            phase: Some(0),
        },
        Case {
            fixture: Fixture {
                pair: (0x07AE_0248, 0x1074_0367),
                state: 0,
                length: 26,
                state_offset: 12,
                flag: None,
            },
            token_offset: 16,
            source_offset: 22,
            target_offset: None,
            variant_offset: 20,
            meaning: Meaning::Activate,
            phase: Some(0),
        },
        Case {
            fixture: Fixture {
                pair: (0x0D8B_032B, 0x188F_044A),
                state: 1,
                length: 29,
                state_offset: 12,
                flag: None,
            },
            token_offset: 16,
            source_offset: 20,
            target_offset: Some(24),
            variant_offset: 28,
            meaning: Meaning::Activate,
            phase: Some(0),
        },
        Case {
            fixture: Fixture {
                pair: (0x10C3_0382, 0x1CE6_04A1),
                state: 1,
                length: 78,
                state_offset: 12,
                flag: None,
            },
            token_offset: 16,
            source_offset: 20,
            target_offset: Some(24),
            variant_offset: 77,
            meaning: Meaning::Launch,
            phase: Some(0),
        },
        Case {
            fixture: Fixture {
                pair: (0x1942_0457, 0x27A3_0576),
                state: 1,
                length: 30,
                state_offset: 12,
                flag: None,
            },
            token_offset: 20,
            source_offset: 16,
            target_offset: Some(24),
            variant_offset: 28,
            meaning: Meaning::Activate,
            phase: Some(0),
        },
        Case {
            fixture: Fixture {
                pair: (0x196B_0451, 0x27CC_0570),
                state: 1,
                length: 29,
                state_offset: 12,
                flag: None,
            },
            token_offset: 24,
            source_offset: 16,
            target_offset: None,
            variant_offset: 28,
            meaning: Meaning::Activate,
            phase: Some(0),
        },
        Case {
            fixture: Fixture {
                pair: (0x196B_0451, 0x27CC_0570),
                state: 2,
                length: 29,
                state_offset: 12,
                flag: None,
            },
            token_offset: 24,
            source_offset: 16,
            target_offset: Some(20),
            variant_offset: 28,
            meaning: Meaning::Impact,
            phase: Some(1),
        },
        Case {
            fixture: Fixture {
                pair: (0x2E54_05E8, 0x4131_0707),
                state: 0,
                length: 26,
                state_offset: 12,
                flag: None,
            },
            token_offset: 16,
            source_offset: 21,
            target_offset: None,
            variant_offset: 20,
            meaning: Meaning::Activate,
            phase: None,
        },
        Case {
            fixture: Fixture {
                pair: (0x2262_0502, 0x3301_0621),
                state: 0,
                length: 30,
                state_offset: 12,
                flag: None,
            },
            token_offset: 16,
            source_offset: 24,
            target_offset: Some(20),
            variant_offset: 29,
            meaning: Meaning::Launch,
            phase: Some(0),
        },
        Case {
            fixture: Fixture {
                pair: (0x3C6F_06D4, 0x518A_07F3),
                state: 1,
                length: 58,
                state_offset: 12,
                flag: None,
            },
            token_offset: 16,
            source_offset: 20,
            target_offset: None,
            variant_offset: 56,
            meaning: Meaning::Launch,
            phase: Some(1),
        },
    ];

    for case in cases {
        let mut operation = raw(case.fixture);
        put_u32(&mut operation, case.token_offset, TOKEN);
        put_u32(&mut operation, case.source_offset, SOURCE);
        if let Some(offset) = case.target_offset {
            put_u32(&mut operation, offset, TARGET);
        }
        operation[case.variant_offset] = 0x5A;
        let consumed = consume(&operation).unwrap();
        assert_eq!(consumed.meaning, case.meaning);
        assert_eq!(consumed.native_phase, case.phase);
        assert_eq!(consumed.transition_token, Some(TOKEN));
        assert_eq!(consumed.source_object_id, Some(SOURCE));
        assert_eq!(
            consumed.target_object_id,
            case.target_offset.map(|_| TARGET)
        );
        assert_eq!(consumed.variant, Some(0x5A));
        assert_same(&operation);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "distinct literals cover every unique actor-binding layout in one audit"
)]
fn independent_oracle_preserves_distinct_actor_offsets_and_ignored_fields() {
    const TOKEN: u32 = 0x7100_0001;
    const TARGET: u32 = 0x7200_0002;
    const SOURCE: u32 = 0x7300_0003;

    let mut coke = raw(Fixture {
        pair: (0x1900_0448, 0x2761_0567),
        state: 4,
        length: 28,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut coke, 16, TOKEN);
    put_u32(&mut coke, 20, TARGET);
    put_u32(&mut coke, 24, SOURCE);
    assert_same(&coke);

    let mut snow = raw(Fixture {
        pair: (0x19EB_046D, 0x284C_058C),
        state: 4,
        length: 28,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut snow, 16, TOKEN);
    put_u32(&mut snow, 20, TARGET);
    put_u32(&mut snow, 24, SOURCE);
    let consumed = consume(&snow).unwrap();
    assert_eq!(consumed.source_object_id, Some(TARGET));
    assert_eq!(consumed.target_object_id, Some(TARGET));
    assert_same(&snow);

    for pair in [
        (0x42E4_071F, 0x591E_083E),
        (0x2954_059D, 0x3B12_06BC),
        (0x41CC_070F, 0x3996_067F),
        (0x2DDA_05D7, 0x40B7_06F6),
    ] {
        let mut operation = raw(Fixture {
            pair,
            state: 2,
            length: 28,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut operation, 16, TOKEN);
        put_u32(&mut operation, 20, TARGET);
        put_u32(&mut operation, 24, SOURCE);
        assert_same(&operation);
    }

    for pair in [(0x2EC5_05FC, 0x41A2_071B), (0x196A_0455, 0x27CB_0574)] {
        let mut operation = raw(Fixture {
            pair,
            state: 3,
            length: 28,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut operation, 16, TOKEN);
        put_u32(&mut operation, 20, TARGET);
        put_u32(&mut operation, 24, SOURCE);
        let consumed = consume(&operation).unwrap();
        assert_eq!(consumed.source_object_id, Some(TARGET));
        assert_eq!(consumed.target_object_id, Some(TARGET));
        assert_same(&operation);
    }

    for (pair, length) in [
        ((0x2954_059D, 0x3B12_06BC), 132),
        ((0x41CC_070F, 0x3996_067F), 24),
        ((0x2DDA_05D7, 0x40B7_06F6), 24),
        ((0x48D7_0757, 0x6030_0876), 24),
        ((0x2EC5_05FC, 0x41A2_071B), 24),
        ((0x196A_0455, 0x27CB_0574), 24),
    ] {
        let mut launch = raw(Fixture {
            pair,
            state: 1,
            length,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut launch, 16, TOKEN);
        put_u32(&mut launch, 20, SOURCE);
        assert_same(&launch);
    }

    for (pair, length) in [
        ((0x2954_059D, 0x3B12_06BC), 24),
        ((0x41CC_070F, 0x3996_067F), 24),
        ((0x2DDA_05D7, 0x40B7_06F6), 24),
    ] {
        let mut resolve = raw(Fixture {
            pair,
            state: 3,
            length,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut resolve, 16, TOKEN);
        put_u32(&mut resolve, 20, TARGET);
        assert_same(&resolve);
    }

    let mut infected_impact = raw(Fixture {
        pair: (0x48D7_0757, 0x6030_0876),
        state: 2,
        length: 32,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut infected_impact, 16, TOKEN);
    put_u32(&mut infected_impact, 20, TARGET);
    put_u32(&mut infected_impact, 24, 0x7400_0004);
    put_u32(&mut infected_impact, 28, SOURCE);
    assert_same(&infected_impact);

    let mut infected_resolve = raw(Fixture {
        pair: (0x48D7_0757, 0x6030_0876),
        state: 3,
        length: 28,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut infected_resolve, 16, TOKEN);
    put_u32(&mut infected_resolve, 20, TARGET);
    put_u32(&mut infected_resolve, 24, 0x7400_0004);
    let consumed = consume(&infected_resolve).unwrap();
    assert_eq!(consumed.source_object_id, None);
    assert_eq!(consumed.target_object_id, Some(TARGET));
    assert_same(&infected_resolve);

    for pair in [(0x2EC5_05FC, 0x41A2_071B), (0x196A_0455, 0x27CB_0574)] {
        let mut impact = raw(Fixture {
            pair,
            state: 2,
            length: 28,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut impact, 16, TOKEN);
        put_u32(&mut impact, 20, TARGET);
        put_u32(&mut impact, 24, SOURCE);
        assert_same(&impact);
    }

    let mut big = raw(Fixture {
        pair: (0x276B_0567, 0x3929_0686),
        state: 4,
        length: 29,
        state_offset: 13,
        flag: None,
    });
    big[12] = 0xA5;
    put_u32(&mut big, 17, TOKEN);
    put_u32(&mut big, 21, TARGET);
    put_u32(&mut big, 25, SOURCE);
    assert_same(&big);

    for absent_offset in [21, 25] {
        let mut missing_actor = big.clone();
        put_u32(&mut missing_actor, absent_offset, u32::MAX);
        let consumed = consume(&missing_actor).unwrap();
        assert_eq!(consumed.native_phase, None);
        assert_eq!(consumed.transition_token, None);
        assert_eq!(consumed.source_object_id, None);
        assert_eq!(consumed.target_object_id, None);
        assert_eq!(consumed.variant, None);
        assert_same(&missing_actor);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "literal sentinels cover each shield, UFO, and LockdownRocket consumer layout"
)]
fn third_pass_oracle_preserves_actor_guards_unaligned_fields_and_phase_selectors() {
    const TOKEN: u32 = 0x8100_0001;
    const TARGET: u32 = 0x8200_0002;
    const SOURCE: u32 = 0x8300_0003;

    for pair in [(0x1457_03C9, 0x2199_04E8), (0x07CF_0250, 0x1095_036F)] {
        let mut activate = raw(Fixture {
            pair,
            state: 1,
            length: 33,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut activate, 16, TOKEN);
        activate[20] = 0xA1;
        put_u32(&mut activate, 21, TARGET);
        put_u32(&mut activate, 25, SOURCE);
        put_u32(&mut activate, 29, 0x8400_0004);
        assert_same(&activate);
    }

    let mut area_resolve = raw(Fixture {
        pair: (0x1457_03C9, 0x2199_04E8),
        state: 2,
        length: 24,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut area_resolve, 16, TOKEN);
    put_u32(&mut area_resolve, 20, TARGET);
    let consumed = consume(&area_resolve).unwrap();
    assert_eq!(consumed.source_object_id, Some(TARGET));
    assert_eq!(consumed.target_object_id, Some(TARGET));
    assert_same(&area_resolve);

    let mut moving_activate = raw(Fixture {
        pair: (0x1E52_04C0, 0x2DD2_05DF),
        state: 1,
        length: 72,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut moving_activate, 16, TOKEN);
    put_u32(&mut moving_activate, 20, SOURCE);
    put_u32(&mut moving_activate, 24, 0x8400_0004);
    assert_same(&moving_activate);

    let mut moving_impact = raw(Fixture {
        pair: (0x1E52_04C0, 0x2DD2_05DF),
        state: 2,
        length: 24,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut moving_impact, 16, TOKEN);
    put_u32(&mut moving_impact, 20, TARGET);
    let consumed = consume(&moving_impact).unwrap();
    assert_eq!(consumed.native_phase, None);
    assert_eq!(consumed.target_object_id, None);
    assert_same(&moving_impact);

    let mut ufo_runtime_only = raw(Fixture {
        pair: (0x07CF_0250, 0x1095_036F),
        state: 2,
        length: 20,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut ufo_runtime_only, 16, TOKEN);
    let consumed = consume(&ufo_runtime_only).unwrap();
    assert_eq!(consumed.transition_token, None);
    assert_same(&ufo_runtime_only);

    let mut shield_activate = raw(Fixture {
        pair: (0x1110_037F, 0x1D33_049E),
        state: 1,
        length: 31,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut shield_activate, 16, TOKEN);
    shield_activate[20..22].copy_from_slice(&0x4404_u16.to_le_bytes());
    put_u32(&mut shield_activate, 22, SOURCE);
    put_u32(&mut shield_activate, 26, 0x8400_0004);
    shield_activate[30] = 0xA2;
    assert_same(&shield_activate);

    let mut shield_impact = raw(Fixture {
        pair: (0x1110_037F, 0x1D33_049E),
        state: 2,
        length: 29,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut shield_impact, 16, TOKEN);
    put_u32(&mut shield_impact, 20, SOURCE);
    put_u32(&mut shield_impact, 24, TARGET);
    shield_impact[28] = 0xA3;
    let consumed = consume(&shield_impact).unwrap();
    assert_eq!(consumed.meaning, Meaning::Impact);
    assert_same(&shield_impact);

    let mut special_activate = raw(Fixture {
        pair: (0x3473_0640, 0x486F_075F),
        state: 0,
        length: 27,
        state_offset: 12,
        flag: None,
    });
    special_activate[16] = 0xA4;
    put_u32(&mut special_activate, 17, TOKEN);
    special_activate[21] = 0xB4;
    put_u32(&mut special_activate, 22, SOURCE);
    special_activate[26] = 0xC4;
    assert_same(&special_activate);

    for state in [2, 3] {
        let mut special_followup = raw(Fixture {
            pair: (0x3473_0640, 0x486F_075F),
            state,
            length: 25,
            state_offset: 12,
            flag: None,
        });
        special_followup[16] = 0xA5;
        put_u32(&mut special_followup, 17, TOKEN);
        put_u32(&mut special_followup, 21, SOURCE);
        assert_same(&special_followup);
    }

    let lockdown_pair = (0x3BEA_06CF, 0x5105_07EE);
    let mut lockdown_launch = raw(Fixture {
        pair: lockdown_pair,
        state: 1,
        length: 20,
        state_offset: 12,
        flag: None,
    });
    lockdown_launch[13] = 0xA6;
    put_u32(&mut lockdown_launch, 14, TOKEN);
    lockdown_launch[18..20].copy_from_slice(&0x4505_u16.to_le_bytes());
    assert_same(&lockdown_launch);

    let mut lockdown_retarget = raw(Fixture {
        pair: lockdown_pair,
        state: 2,
        length: 17,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut lockdown_retarget, 13, TARGET);
    assert_same(&lockdown_retarget);

    for state in [4, 9] {
        let mut actor_transition = raw(Fixture {
            pair: lockdown_pair,
            state,
            length: 25,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut actor_transition, 13, TOKEN);
        put_u32(&mut actor_transition, 17, SOURCE);
        put_u32(&mut actor_transition, 21, TARGET);
        assert_same(&actor_transition);
    }

    for state in [5, 6] {
        let mut conditional = raw(Fixture {
            pair: lockdown_pair,
            state,
            length: 18,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut conditional, 13, TOKEN);
        conditional[17] = 1;
        let consumed = consume(&conditional).unwrap();
        assert_eq!(consumed.native_phase, None);
        assert_same(&conditional);
    }

    for (state, zero_phase, nonzero_phase) in [(7, 7, 8), (8, 11, 10)] {
        for (variant, expected_phase) in [(0, zero_phase), (1, nonzero_phase)] {
            let mut selected = raw(Fixture {
                pair: lockdown_pair,
                state,
                length: 26,
                state_offset: 12,
                flag: None,
            });
            put_u32(&mut selected, 13, TOKEN);
            put_u32(&mut selected, 17, SOURCE);
            put_u32(&mut selected, 21, TARGET);
            selected[25] = variant;
            assert_eq!(
                consume(&selected).unwrap().native_phase,
                Some(expected_phase)
            );
            assert_same(&selected);
        }
    }
}

#[test]
fn thunderbolt_oracle_preserves_counted_targets_and_guarded_impact_bindings() {
    const TOKEN: u32 = 0xA100_0001;
    const TARGET: u32 = 0xA200_0002;
    const SOURCE: u32 = 0xA300_0003;
    const TARGETS: [u32; 3] = [0xA400_0004, 0xA500_0005, 0xA600_0006];
    const PAIR: (u32, u32) = (0x2973_05B1, 0x3B31_06D0);

    let mut activate = raw(Fixture {
        pair: PAIR,
        state: 1,
        length: 42,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut activate, 16, TOKEN);
    activate[20] = 0xA7;
    put_u32(&mut activate, 21, SOURCE);
    put_u32(&mut activate, 25, 3);
    for (index, target) in TARGETS.into_iter().enumerate() {
        put_u32(&mut activate, 29 + index * 4, target);
    }
    activate[41] = 0xB7;
    let consumed = consume(&activate).unwrap();
    assert_eq!(consumed.target_object_ids, TARGETS);
    assert_eq!(consumed.variant, Some(0xB7));
    assert_same(&activate);
    assert!(consume(&activate[..41]).is_err());
    assert!(!server_decodes_strict_item_operation(&activate[..41]));
    let mut count_three_with_suffix = activate.clone();
    count_three_with_suffix.push(0xC7);
    assert!(consume(&count_three_with_suffix).is_err());
    assert!(!server_decodes_strict_item_operation(
        &count_three_with_suffix
    ));

    for (state, length, expected_phase) in [(2, 25, 4), (3, 29, 3)] {
        let mut impact = raw(Fixture {
            pair: PAIR,
            state,
            length,
            state_offset: 12,
            flag: None,
        });
        impact[16] = 0xA8;
        put_u32(&mut impact, 17, TOKEN);
        put_u32(&mut impact, 21, TARGET);
        if state == 3 {
            put_u32(&mut impact, 25, SOURCE);
        }
        assert_eq!(consume(&impact).unwrap().native_phase, Some(expected_phase));
        assert_same(&impact);

        put_u32(&mut impact, 21, u32::MAX);
        let missing = consume(&impact).unwrap();
        assert_eq!(missing.native_phase, None);
        assert_eq!(missing.transition_token, None);
        assert_eq!(missing.source_object_id, None);
        assert_eq!(missing.target_object_id, None);
        assert_same(&impact);
    }

    let zero_targets = raw(Fixture {
        pair: PAIR,
        state: 1,
        length: 30,
        state_offset: 12,
        flag: None,
    });
    assert!(consume(&zero_targets).unwrap().target_object_ids.is_empty());
    assert_same(&zero_targets);

    for invalid_count in [1, 233, u32::MAX] {
        let mut invalid = zero_targets.clone();
        put_u32(&mut invalid, 25, invalid_count);
        assert!(consume(&invalid).is_err());
        assert!(!server_decodes_strict_item_operation(&invalid));
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "literal sentinels prove the ordinary-effect actor-role switches and flag guards"
)]
fn ordinary_effect_oracle_preserves_source_target_roles_and_runtime_only_states() {
    const TOKEN: u32 = 0xB100_0001;
    const TARGET: u32 = 0xB200_0002;
    const SOURCE: u32 = 0xB300_0003;

    for (pair, place_length, source_offset, variant_offset) in [
        ((0x1DC6_04B1, 0x2D46_05D0), 72, 68, None),
        ((0x07C0_024A, 0x1086_0369), 73, 69, Some(20)),
    ] {
        let mut place = raw(Fixture {
            pair,
            state: 1,
            length: place_length,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut place, 16, TOKEN);
        put_u32(&mut place, source_offset, SOURCE);
        if let Some(offset) = variant_offset {
            place[offset] = 0xC1;
        }
        assert_same(&place);

        let mut impact = raw(Fixture {
            pair,
            state: 2,
            length: 29,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut impact, 16, TOKEN);
        put_u32(&mut impact, 20, TARGET);
        impact[24] = 1;
        put_u32(&mut impact, 25, SOURCE);
        let consumed = consume(&impact).unwrap();
        assert_eq!(consumed.meaning, Meaning::Impact);
        assert_eq!(consumed.native_phase, Some(2));
        assert_eq!(consumed.source_object_id, Some(SOURCE));
        assert_eq!(consumed.target_object_id, Some(TARGET));
        assert_same(&impact);

        impact[24] = 0;
        let failed = consume(&impact).unwrap();
        assert_eq!(
            failed.meaning,
            if pair == (0x1DC6_04B1, 0x2D46_05D0) {
                Meaning::Resolve
            } else {
                Meaning::Remove
            }
        );
        assert_eq!(failed.native_phase, None);
        assert_eq!(failed.source_object_id, Some(SOURCE));
        assert_eq!(failed.target_object_id, None);
        assert_same(&impact);
    }

    let mut force_resolve = raw(Fixture {
        pair: (0x1DC6_04B1, 0x2D46_05D0),
        state: 3,
        length: 29,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut force_resolve, 16, TOKEN);
    put_u32(&mut force_resolve, 20, SOURCE);
    force_resolve[24] = 1;
    put_u32(&mut force_resolve, 25, TARGET);
    assert_same(&force_resolve);

    for (pair, length, expected_meaning) in [
        ((0x1DC6_04B1, 0x2D46_05D0), 29, Meaning::Resolve),
        ((0x07C0_024A, 0x1086_0369), 25, Meaning::Remove),
    ] {
        let mut zero_result = raw(Fixture {
            pair,
            state: 3,
            length,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut zero_result, 16, TOKEN);
        put_u32(&mut zero_result, 20, SOURCE);
        if pair == (0x1DC6_04B1, 0x2D46_05D0) {
            put_u32(&mut zero_result, 25, TARGET);
        }
        let consumed = consume(&zero_result).unwrap();
        assert_eq!(consumed.meaning, expected_meaning);
        assert_eq!(consumed.native_phase, None);
        assert_eq!(consumed.source_object_id, None);
        assert_eq!(consumed.target_object_id, None);
        assert_same(&zero_result);
    }

    let mut force_special = raw(Fixture {
        pair: (0x1DC6_04B1, 0x2D46_05D0),
        state: 5,
        length: 25,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut force_special, 16, TOKEN);
    put_u32(&mut force_special, 20, SOURCE);
    force_special[24] = 1;
    let consumed = consume(&force_special).unwrap();
    assert_eq!(consumed.native_phase, Some(5));
    assert_eq!(consumed.transition_token, None);
    assert_same(&force_special);

    let mut silence = raw(Fixture {
        pair: (0x150D_03E9, 0x224F_0508),
        state: 1,
        length: 29,
        state_offset: 12,
        flag: None,
    });
    silence[16] = 0xC2;
    put_u32(&mut silence, 17, TOKEN);
    put_u32(&mut silence, 21, SOURCE);
    put_u32(&mut silence, 25, TARGET);
    assert_same(&silence);
    put_u32(&mut silence, 12, 2);
    assert_eq!(consume(&silence).unwrap().meaning, Meaning::NoClientAction);
    assert_same(&silence);

    let mut siren_activate = raw(Fixture {
        pair: (0x0DB2_0327, 0x18B6_0446),
        state: 1,
        length: 26,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut siren_activate, 16, TOKEN);
    siren_activate[20] = 0xC3;
    put_u32(&mut siren_activate, 21, SOURCE);
    siren_activate[25] = 0xD3;
    assert_same(&siren_activate);

    let mut siren_impact = raw(Fixture {
        pair: (0x0DB2_0327, 0x18B6_0446),
        state: 2,
        length: 31,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut siren_impact, 16, TOKEN);
    put_u32(&mut siren_impact, 20, TARGET);
    put_u32(&mut siren_impact, 24, SOURCE);
    siren_impact[28] = 0xC4;
    siren_impact[29..31].copy_from_slice(&0x4606_u16.to_le_bytes());
    assert_same(&siren_impact);

    for (state, length, expected_phase) in [(0, 25, 0), (2, 25, 2)] {
        let mut shield = raw(Fixture {
            pair: (0x28A5_0580, 0x3A63_069F),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut shield, 16, TOKEN);
        shield[20] = 0xC5;
        put_u32(&mut shield, 21, SOURCE);
        assert_eq!(consume(&shield).unwrap().native_phase, Some(expected_phase));
        assert_same(&shield);
    }
    let mut shield_hit = raw(Fixture {
        pair: (0x28A5_0580, 0x3A63_069F),
        state: 1,
        length: 24,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut shield_hit, 16, TOKEN);
    put_u32(&mut shield_hit, 20, TARGET);
    let consumed = consume(&shield_hit).unwrap();
    assert_eq!(consumed.source_object_id, Some(TARGET));
    assert_eq!(consumed.target_object_id, Some(TARGET));
    assert_same(&shield_hit);

    for (state, length, phase, variant_offset) in [(0, 30, 0, 29), (1, 29, 3, 28)] {
        let mut small = raw(Fixture {
            pair: (0x2E3D_05E0, 0x411A_06FF),
            state,
            length,
            state_offset: 12,
            flag: None,
        });
        put_u32(&mut small, 16, TOKEN);
        put_u32(&mut small, 20, TARGET);
        put_u32(&mut small, 24, SOURCE);
        small[variant_offset] = 0xC6;
        assert_eq!(consume(&small).unwrap().native_phase, Some(phase));
        assert_same(&small);
    }
    let mut runtime_flag = raw(Fixture {
        pair: (0x2E3D_05E0, 0x411A_06FF),
        state: 2,
        length: 17,
        state_offset: 12,
        flag: None,
    });
    runtime_flag[16] = 1;
    let consumed = consume(&runtime_flag).unwrap();
    assert_eq!(consumed.meaning, Meaning::UpdateRuntimeFlag);
    assert_eq!(consumed.native_phase, None);
    assert_eq!(consumed.transition_token, None);
    assert_same(&runtime_flag);
}

#[test]
fn unresolved_straight_rocket_writer_shapes_remain_unknown() {
    for fixture in [
        Fixture {
            pair: (0x3C6F_06D4, 0x518A_07F3),
            state: 2,
            length: 24,
            state_offset: 12,
            flag: None,
        },
        Fixture {
            pair: (0x3C6F_06D4, 0x518A_07F3),
            state: 3,
            length: 24,
            state_offset: 12,
            flag: None,
        },
    ] {
        let consumed = consume(&raw(fixture)).unwrap();
        assert_eq!(consumed.meaning, Meaning::Unknown);
        assert_eq!(consumed.native_phase, None);
        assert_eq!(consumed.transition_token, None);
        assert_eq!(consumed.source_object_id, None);
        assert_eq!(consumed.target_object_id, None);
        assert_same(&raw(fixture));
    }
}

#[test]
fn angel_state_two_is_a_proven_non_terminal_defense_impact() {
    const TOKEN: u32 = 0xA110_0001;
    const SOURCE: u32 = 0xA220_0002;
    const TARGET: u32 = 0xA330_0003;

    let fixture = Fixture {
        pair: (0x0D49_030D, 0x184D_042C),
        state: 2,
        length: 28,
        state_offset: 12,
        flag: None,
    };
    let mut operation = raw(fixture);
    put_u32(&mut operation, 16, TOKEN);
    put_u32(&mut operation, 20, SOURCE);
    put_u32(&mut operation, 24, TARGET);

    let consumed = consume(&operation).unwrap();
    assert_eq!(consumed.meaning, Meaning::Impact);
    assert_eq!(consumed.native_phase, Some(2));
    assert_eq!(consumed.transition_token, Some(TOKEN));
    assert_eq!(consumed.source_object_id, Some(SOURCE));
    assert_eq!(consumed.target_object_id, Some(TARGET));
    assert_same(&operation);
}

#[test]
fn fifth_pass_oracle_preserves_actor_roles_and_native_asymmetries() {
    for (pair, state, length, phase) in [
        ((0x1977_0461, 0x27D8_0580), 2, 29, 3),
        ((0x10D3_0380, 0x1CF6_049F), 2, 29, 3),
        ((0x0DC1_0333, 0x18C5_0452), 2, 28, 5),
    ] {
        let fixture = Fixture {
            pair,
            state,
            length,
            state_offset: 12,
            flag: None,
        };
        let mut operation = raw(fixture);
        put_u32(&mut operation, 16, 0xA111_0001);
        put_u32(&mut operation, 20, 0xA222_0002);
        put_u32(&mut operation, 24, 0xA333_0003);
        let consumed = consume(&operation).unwrap();
        assert_eq!(consumed.transition_token, Some(0xA111_0001));
        assert_eq!(consumed.source_object_id, Some(0xA222_0002));
        assert_eq!(consumed.target_object_id, Some(0xA333_0003));
        assert_eq!(consumed.native_phase, Some(phase));
        assert_same(&operation);
    }

    for (pair, phase) in [
        ((0x1DC5_04A1, 0x2D45_05C0), 2),
        ((0x1E29_04C1, 0x2DA9_05E0), 1),
    ] {
        let fixture = Fixture {
            pair,
            state: 2,
            length: 24,
            state_offset: 12,
            flag: None,
        };
        let mut operation = raw(fixture);
        put_u32(&mut operation, 16, 0xB111_0001);
        put_u32(&mut operation, 20, 0xB222_0002);
        let consumed = consume(&operation).unwrap();
        assert_eq!(consumed.meaning, Meaning::Unknown);
        assert_eq!(consumed.native_phase, Some(phase));
        assert_eq!(consumed.transition_token, Some(0xB111_0001));
        assert_eq!(consumed.source_object_id, None);
        assert_eq!(consumed.target_object_id, Some(0xB222_0002));
        assert_same(&operation);
    }

    for pair in [(0x3442_0652, 0x483E_0771), (0x42B8_070F, 0x58F2_082E)] {
        let fixture = Fixture {
            pair,
            state: 4,
            length: 28,
            state_offset: 12,
            flag: None,
        };
        let mut operation = raw(fixture);
        put_u32(&mut operation, 16, 0xC111_0001);
        put_u32(&mut operation, 20, 0xC222_0002);
        put_u32(&mut operation, 24, 0xC333_0003);
        let consumed = consume(&operation).unwrap();
        assert_eq!(consumed.meaning, Meaning::Unknown);
        assert_eq!(consumed.native_phase, Some(4));
        assert_eq!(consumed.transition_token, Some(0xC111_0001));
        assert_eq!(consumed.target_object_id, Some(0xC222_0002));
        assert_eq!(consumed.source_object_id, Some(0xC333_0003));
        assert_same(&operation);
    }
}

#[test]
fn sixth_pass_oracle_recovers_course_hazard_and_pirate_occurrence_semantics() {
    let mut goal = vec![0_u8; 32];
    put_u32(&mut goal, 0, 0x1139_0397);
    put_u32(&mut goal, 4, 0x0D73_0327);
    put_u32(&mut goal, 8, 0x7000_0001);
    put_u32(&mut goal, 12, 0xD001_0001);
    put_u32(&mut goal, 16, 4);
    goal[20..28].copy_from_slice(&[b'g', 0, b'o', 0, b'a', 0, b'l', 0]);
    put_u32(&mut goal, 28, 0xD002_0002);
    let consumed = consume(&goal).unwrap();
    assert_eq!(consumed.class_name, "GopCourse");
    assert_eq!(consumed.state, 0xD001_0001);
    assert_eq!(consumed.meaning, Meaning::NoClientAction);
    assert_eq!(consumed.target_object_id, Some(0xD001_0001));
    assert_eq!(consumed.transition_token, Some(0xD002_0002));
    assert_same(&goal);

    for length in 0..goal.len() {
        assert!(consume(&goal[..length]).is_err());
        assert!(!server_decodes_strict_item_operation(&goal[..length]));
    }
    let mut extended_goal = goal.clone();
    extended_goal.push(0xA5);
    assert!(consume(&extended_goal).is_err());
    assert!(!server_decodes_strict_item_operation(&extended_goal));

    for (pair, state, length, state_offset, source_offset, phase) in [
        ((0x233A_0538, 0x33D9_0657), 1, 77, 12, 20, 0),
        ((0x1DB9_04A4, 0x2D39_05C3), 1, 63, 16, 53, 0),
        ((0x14A7_03E3, 0x21E9_0502), 1, 91, 16, 85, 0),
        ((0x2369_052B, 0x3408_064A), 1, 28, 12, 20, 0),
    ] {
        let mut operation = raw(Fixture {
            pair,
            state,
            length,
            state_offset,
            flag: None,
        });
        put_u32(&mut operation, source_offset, 0xE001_0001);
        let consumed = consume(&operation).unwrap();
        assert_eq!(consumed.source_object_id, Some(0xE001_0001));
        assert_eq!(consumed.native_phase, Some(phase));
        assert_same(&operation);
    }

    for (pair, phase, restores_source) in [
        ((0x1DB9_04A4, 0x2D39_05C3), 2, false),
        ((0x14A7_03E3, 0x21E9_0502), 3, true),
    ] {
        let mut operation = raw(Fixture {
            pair,
            state: 2,
            length: 33,
            state_offset: 16,
            flag: Some(1),
        });
        put_u32(&mut operation, 20, 0xE002_0002);
        put_u32(&mut operation, 24, 0xE003_0003);
        put_u32(&mut operation, 29, 0xE004_0004);
        let consumed = consume(&operation).unwrap();
        assert_eq!(consumed.meaning, Meaning::Impact);
        assert_eq!(consumed.native_phase, Some(phase));
        assert_eq!(consumed.transition_token, Some(0xE002_0002));
        assert_eq!(consumed.target_object_id, Some(0xE003_0003));
        assert_eq!(
            consumed.source_object_id,
            restores_source.then_some(0xE004_0004)
        );
        assert_same(&operation);
    }

    let mut shielded = raw(Fixture {
        pair: (0x2369_052B, 0x3408_064A),
        state: 4,
        length: 28,
        state_offset: 12,
        flag: None,
    });
    put_u32(&mut shielded, 20, 0xE005_0005);
    put_u32(&mut shielded, 24, 0xE006_0006);
    let consumed = consume(&shielded).unwrap();
    assert_eq!(consumed.meaning, Meaning::Resolve);
    assert_eq!(consumed.source_object_id, Some(0xE006_0006));
    assert_eq!(consumed.target_object_id, Some(0xE006_0006));
    assert_same(&shielded);
}

#[test]
fn independent_oracle_rejects_shape_drift() {
    for fixture in all_recovered_fixtures() {
        let exact = raw(fixture);
        for length in 0..exact.len() {
            assert!(consume(&exact[..length]).is_err());
            assert!(!server_decodes_strict_item_operation(&exact[..length]));
        }
        let mut extended = exact;
        extended.push(0xA5);
        assert!(consume(&extended).is_err());
        assert!(!server_decodes_strict_item_operation(&extended));
    }
}
