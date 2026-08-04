#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    IdbLayoutExactPartialSemantics,
    IdbCodecAndConsumerExact,
    IdbPartialPlusDeployedTrace,
    StockConsumerBranch,
    CSharpGoldenPlusLiveTrace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evidence {
    pub packet: &'static str,
    pub hash: u32,
    pub source_anchor: &'static str,
    pub artifact: &'static str,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionConfidence {
    NativeConsumerAndStateEffect,
    NativeConsumerPlusTrace,
    DeployedCompatibilityTrace,
    RuntimeFailureBoundary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionEvidence {
    pub transition: &'static str,
    pub source_anchor: &'static str,
    pub artifact: &'static str,
    pub confidence: TransitionConfidence,
}

/// Evidence for the compatibility FSM. These rows concern sequencing and
/// scene effects; they are intentionally separate from byte-codec coverage.
pub const FSM_AUDITED: &[TransitionEvidence] = &[
    TransitionEvidence {
        transition: "server-first handshake -> encrypted login",
        source_anchor: "PcFirstMessage retained encrypted-TCP progression",
        artifact: "PORTING_STATUS.md#Audited-login-and-menu-initialization-path",
        confidence: TransitionConfidence::NativeConsumerPlusTrace,
    },
    TransitionEvidence {
        transition: "normal channel switch -> reconnect -> channel move-in",
        source_anchor: "sub_BEBF70 plus migration integration trace",
        artifact: "analysis/ida_5136_protocol_fsm_transitions.log",
        confidence: TransitionConfidence::NativeConsumerPlusTrace,
    },
    TransitionEvidence {
        transition: "room lobby -> GrCommandStart -> loading",
        source_anchor: "sub_CF3D10 / sub_CF3F30",
        artifact: "analysis/ida_5136_protocol_fsm_transitions.log",
        confidence: TransitionConfidence::NativeConsumerAndStateEffect,
    },
    TransitionEvidence {
        transition: "GameControl states 1 -> 3 -> 4",
        source_anchor: "sub_A847F0 virtual slots 97/98/99",
        artifact: "analysis/ida_5136_protocol_fsm_control.log",
        confidence: TransitionConfidence::NativeConsumerAndStateEffect,
    },
    TransitionEvidence {
        transition: "state 4 -> GameNextStage -> GameResult",
        source_anchor: "known-working deployed sequences 2161..2176",
        artifact: "PORTING_STATUS.md#Ceremony-packet-order-correction-from-deployed-C-evidence",
        confidence: TransitionConfidence::DeployedCompatibilityTrace,
    },
    TransitionEvidence {
        transition: "podium scheduler -> final-stage slot 103 -> ready stage",
        source_anchor: "sub_B42500 / sub_B507D0 / sub_B49BB0 / sub_BED1D0",
        artifact: "analysis/ida_5136_next_stage_command_probe.log",
        confidence: TransitionConfidence::NativeConsumerAndStateEffect,
    },
    TransitionEvidence {
        transition: "loading UDP time-sync is readiness evidence, not a state-1 guard",
        source_anchor: "two-machine eager-ready falsification",
        artifact: "PORTING_STATUS.md#LAN-relay-readiness",
        confidence: TransitionConfidence::NativeConsumerPlusTrace,
    },
    TransitionEvidence {
        transition: "standalone GrSlotData is rejected after lobby",
        source_anchor: "post-ceremony stale snapshot reset",
        artifact: "PORTING_STATUS.md#Post-ceremony-stale-lobby-snapshot-crash-fix",
        confidence: TransitionConfidence::RuntimeFailureBoundary,
    },
    TransitionEvidence {
        transition: "149 item consumers -> local/deferred/unknown side-effect outcome",
        source_anchor: "Gop concrete consumers joined with recovered producer continuations",
        artifact: "analysis/P5136_ITEM_OPERATION_SEMANTICS.md",
        confidence: TransitionConfidence::NativeConsumerAndStateEffect,
    },
];

/// One evidence row per decoded packet in the first oracle slice. Paths name
/// the private analysis artifacts; neither those artifacts nor the client
/// binary are copied into this public crate.
pub const AUDITED: &[Evidence] = &[
    Evidence {
        packet: "GameSlotPacket type-12 item consumers (63-class expansion)",
        hash: 0x27C0_0574,
        source_anchor: "Gop writers plus RTTI-reached GoItem runtime consumers",
        artifact: "analysis/P5136_ITEM_OPERATION_SEMANTICS.md",
        confidence: Confidence::IdbCodecAndConsumerExact,
    },
    Evidence {
        packet: "GameResultPacket",
        hash: 0x345C_0651,
        source_anchor: "sub_726CC0 / sub_71BF00 / sub_71BAD0",
        artifact: "analysis/ida_5136_podium_runtime.log",
        confidence: Confidence::IdbLayoutExactPartialSemantics,
    },
    Evidence {
        packet: "GameNextStagePacket",
        hash: 0x4891_0765,
        source_anchor: "sub_72FC40 plus stage consumer",
        artifact: "analysis/ida_5136_podium_runtime.log",
        confidence: Confidence::IdbCodecAndConsumerExact,
    },
    Evidence {
        packet: "GameControlPacket",
        hash: 0x3ACB_06B3,
        source_anchor: "partial native consumer plus deployed ceremony trace",
        artifact: "analysis/reverse_2005/game_control_ida_dump.txt",
        confidence: Confidence::IdbPartialPlusDeployedTrace,
    },
    Evidence {
        packet: "ChGetRoomListReplyPacket",
        hash: 0x7286_0968,
        source_anchor: "C# writer plus live room-list progression",
        artifact: "analysis/reverse_2005/roomlist_ida_dump.txt",
        confidence: Confidence::CSharpGoldenPlusLiveTrace,
    },
    Evidence {
        packet: "ChCreateRoomReplyPacket",
        hash: 0x6937_0900,
        source_anchor: "C# writer plus live room-admission progression",
        artifact: "analysis/reverse_2005/joinroom_ida_dump.txt",
        confidence: Confidence::CSharpGoldenPlusLiveTrace,
    },
    Evidence {
        packet: "ChJoinRoomReplyPacket",
        hash: 0x584A_083C,
        source_anchor: "C# writer plus live room-admission progression",
        artifact: "analysis/reverse_2005/joinroom_ida_dump.txt",
        confidence: Confidence::CSharpGoldenPlusLiveTrace,
    },
    Evidence {
        packet: "GrSessionDataPacket",
        hash: 0x498E_076F,
        source_anchor: "C# writer plus live initial-room progression",
        artifact: "analysis/reverse_2005/joinroom_ida_dump.txt",
        confidence: Confidence::CSharpGoldenPlusLiveTrace,
    },
    Evidence {
        packet: "GrSlotDataPacket",
        hash: 0x337C_062D,
        source_anchor: "C# writer plus live initial-room progression",
        artifact: "analysis/reverse_2005/joinroom_ida_dump.txt",
        confidence: Confidence::CSharpGoldenPlusLiveTrace,
    },
    Evidence {
        packet: "PrCnAuthenLogin",
        hash: 0x2D30_05D1,
        source_anchor: "C# writer plus live authentication progression",
        artifact: "analysis/reverse_2005/team_semantics_ida_dump.txt",
        confidence: Confidence::CSharpGoldenPlusLiveTrace,
    },
    Evidence {
        packet: "PrLogin",
        hash: 0x0A89_02BB,
        source_anchor: "C# writer plus live login progression",
        artifact: "analysis/reverse_2005/team_semantics_ida_dump.txt",
        confidence: Confidence::CSharpGoldenPlusLiveTrace,
    },
    Evidence {
        packet: "PrChannelMoveIn",
        hash: 0x2DA4_05C9,
        source_anchor: "C# writer plus live migration progression",
        artifact: "analysis/reverse_2005/team_semantics_ida_dump.txt",
        confidence: Confidence::CSharpGoldenPlusLiveTrace,
    },
    Evidence {
        packet: "PrCheckMyClubStatePacket",
        hash: 0x718B_0945,
        source_anchor: "stock membership-gate consumer",
        artifact: "analysis/P5136_CSHARP_OPAQUE_PACKET_AUDIT.md",
        confidence: Confidence::StockConsumerBranch,
    },
    Evidence {
        packet: "PrGetUserWaitingJoinClubPacket",
        hash: 0xB4E2_0BC2,
        source_anchor: "stock pending-join consumer",
        artifact: "analysis/P5136_CSHARP_OPAQUE_PACKET_AUDIT.md",
        confidence: Confidence::StockConsumerBranch,
    },
    Evidence {
        packet: "PrCheckCreateClubConditionPacket",
        hash: 0xC998_0C79,
        source_anchor: "stock create-condition consumer",
        artifact: "analysis/P5136_CSHARP_OPAQUE_PACKET_AUDIT.md",
        confidence: Confidence::StockConsumerBranch,
    },
    Evidence {
        packet: "PrGetClubListCountPacket",
        hash: 0x72E0_0965,
        source_anchor: "stock list-count consumer",
        artifact: "analysis/P5136_CSHARP_OPAQUE_PACKET_AUDIT.md",
        confidence: Confidence::StockConsumerBranch,
    },
    Evidence {
        packet: "PrGetClubWaitingCrewCountPacket",
        hash: 0xBF7C_0C2D,
        source_anchor: "stock capacity consumer",
        artifact: "analysis/P5136_CSHARP_OPAQUE_PACKET_AUDIT.md",
        confidence: Confidence::StockConsumerBranch,
    },
];
