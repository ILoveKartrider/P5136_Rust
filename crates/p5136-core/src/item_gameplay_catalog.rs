//! Gameplay-level item reference for the 54 entries in the supplied Korean
//! item page.
//!
//! This is deliberately separate from the type-12 wire decoder. The page was
//! last edited in 2026 and spans versions newer than P5136, so its prose is a
//! useful naming/target/effect hint but cannot prove packet offsets, state
//! numbers, timers, probabilities, or P5136 availability. Only executable or
//! retained configuration evidence is allowed to upgrade a protocol link.

use crate::game_slot_item_schema::{ItemOperationClassEvidence, item_operation_class_evidence};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameplayItemCategory {
    Acceleration,
    Attack,
    Defense,
    Placement,
    Status,
    Utility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameplayTargetScope {
    SelfKart,
    ContactedKart,
    AimedKart,
    FirstPlaceKart,
    RandomAheadOpponent,
    NearestAheadOpponent,
    AheadOpponents,
    NearbyOtherKarts,
    NearbyKartsIncludingSource,
    OpposingTeam,
    OwnTeam,
    NonAlliedKarts,
    TrackArea,
    EveryoneExceptSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameplayEffectHint {
    SpeedBoost,
    ContactSpin,
    CrushKart,
    PullTowardTarget,
    TemporaryShield,
    LaunchAirborne,
    SlowKart,
    ObscureVision,
    TrapInWater,
    LockItemSlots,
    ElectromagneticField,
    ShrinkKart,
    PlaceRollingBomb,
    DelayedAreaWaterBomb,
    Immobilize,
    UfoSlow,
    LightningStrike,
    RemoveUfo,
    PlaceVisionCloud,
    SpinKart,
    PlaceMine,
    Knockback,
    ReverseSteering,
    ReverseThrottle,
    RevealItemSlots,
    HideKart,
    OilBlind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemSymbolEvidence {
    /// Numeric/name pair retained in the P5136 safe probability table.
    P5136FallbackTable,
    /// Numeric/name pair recovered from the Korean P5136 executable.
    P5136ExecutableInitializer,
    /// Numeric/name pair retained as an independently verified P5136 profile
    /// supplement when catalog generation cannot recover it automatically.
    P5136VerifiedSupplement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ItemSymbolLink {
    pub item_id: i16,
    pub symbol: &'static str,
    pub evidence: ItemSymbolEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationLinkEvidence {
    /// The page heading is directly associated with this P5136 class. Native
    /// class recovery remains a separate evidence axis.
    VerifiedAssociation,
    /// The class exists in P5136, but the page-to-class association is based
    /// only on naming/effect correlation.
    NameCorrelation,
    /// More than one P5136 class can plausibly represent this page entry.
    AmbiguousCandidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperationLink {
    pub class_name: &'static str,
    pub evidence: OperationLinkEvidence,
}

impl OperationLink {
    /// Evidence that the named class itself exists in the bounded P5136
    /// operation family, independent of the page-heading association.
    #[must_use]
    pub fn class_evidence(self) -> Option<ItemOperationClassEvidence> {
        item_operation_class_evidence(self.class_name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameplayAvailabilityHint {
    SpeedTeam,
    FlagIndividual,
    FlagTeam,
    ItemTeam,
    ItemTeamBattle,
    ReverseTrackOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameplayItemHint {
    pub slug: &'static str,
    pub korean_name: &'static str,
    pub category: GameplayItemCategory,
    /// Short paraphrase of the supplied page. It is not an authoritative
    /// P5136 duration or balance rule.
    pub effect_summary_ko: &'static str,
    pub targets: &'static [GameplayTargetScope],
    pub effects: &'static [GameplayEffectHint],
    pub item_symbols: &'static [ItemSymbolLink],
    pub operation_links: &'static [OperationLink],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum P5136CoverageLevel {
    GameplayReferenceOnly,
    OperationCandidate,
    VerifiedItemSymbol,
    VerifiedOperation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReverseEngineeringScope {
    InScope,
    DeferredByUser,
}

impl GameplayItemHint {
    /// Current reverse-engineering scope. Deferred entries remain in the
    /// complete 54-heading reference and keep their existing evidence; this
    /// flag only prevents them from being counted as active ambiguity work.
    #[must_use]
    pub fn reverse_engineering_scope(self) -> ReverseEngineeringScope {
        match self.slug {
            "rolling_waterbomb" | "jiangshi" | "first_place_devil" => {
                ReverseEngineeringScope::DeferredByUser
            }
            _ => ReverseEngineeringScope::InScope,
        }
    }

    /// Mode constraints explicitly recorded from the modern gameplay page.
    /// These are reference metadata only and are deliberately not encoded as
    /// target scopes. `None` means unrecorded, not unrestricted.
    #[must_use]
    pub fn documented_availability(self) -> Option<&'static [GameplayAvailabilityHint]> {
        match self.slug {
            "power_booster" => Some(&[GameplayAvailabilityHint::SpeedTeam]),
            "rocket_launcher" => Some(&[
                GameplayAvailabilityHint::FlagIndividual,
                GameplayAvailabilityHint::FlagTeam,
            ]),
            "random_missile" => Some(&[GameplayAvailabilityHint::ItemTeam]),
            "doctor_r" => Some(&[GameplayAvailabilityHint::ReverseTrackOnly]),
            "slot_lock" | "scanning" => Some(&[
                GameplayAvailabilityHint::ItemTeam,
                GameplayAvailabilityHint::ItemTeamBattle,
            ]),
            _ => None,
        }
    }

    /// Strongest P5136 evidence attached to this gameplay-page entry. This is
    /// a coverage grade, not a claim that later-version balance behavior is
    /// present in P5136.
    #[must_use]
    pub fn p5136_coverage(self) -> P5136CoverageLevel {
        if self.operation_links.iter().any(|link| {
            link.evidence == OperationLinkEvidence::VerifiedAssociation
                && link.class_evidence() == Some(ItemOperationClassEvidence::NativeWriterSchema)
        }) {
            P5136CoverageLevel::VerifiedOperation
        } else if !self.item_symbols.is_empty() {
            P5136CoverageLevel::VerifiedItemSymbol
        } else if !self.operation_links.is_empty() {
            P5136CoverageLevel::OperationCandidate
        } else {
            P5136CoverageLevel::GameplayReferenceOnly
        }
    }
}

pub const GAMEPLAY_REFERENCE_TITLE: &str = "크레이지레이싱 카트라이더/아이템";
pub const GAMEPLAY_REFERENCE_LAST_EDITED: &str = "2026-07-11 00:02:47";
pub const GAMEPLAY_REFERENCE_SHA256: &str =
    "51501a82e6d78a759270d69eea1bde08eda5bb77db91fb95cd02590e73e22d1b";

const FALLBACK: ItemSymbolEvidence = ItemSymbolEvidence::P5136FallbackTable;
const EXECUTABLE: ItemSymbolEvidence = ItemSymbolEvidence::P5136ExecutableInitializer;
const SUPPLEMENT: ItemSymbolEvidence = ItemSymbolEvidence::P5136VerifiedSupplement;
const VERIFIED: OperationLinkEvidence = OperationLinkEvidence::VerifiedAssociation;
const NAMED: OperationLinkEvidence = OperationLinkEvidence::NameCorrelation;
const AMBIGUOUS: OperationLinkEvidence = OperationLinkEvidence::AmbiguousCandidate;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationItemSelector {
    FixedClass,
    ItemIdU16 { raw_offset: u8 },
    VariantByte { raw_offset: u8, value: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecoveredOperationItemAssociation {
    pub class_name: &'static str,
    pub item_id: i16,
    pub symbol: &'static str,
    pub selector: OperationItemSelector,
}

/// Executable- and `item.rho`-cross-checked operation/item joins relevant to
/// the formerly ambiguous gameplay rows. This intentionally also records the
/// distinct battle/special items that caused the original name collisions.
pub static P5136_RECOVERED_OPERATION_ITEM_ASSOCIATIONS: &[RecoveredOperationItemAssociation] = &[
    RecoveredOperationItemAssociation {
        class_name: "GopRocket",
        item_id: 7,
        symbol: "rocket",
        selector: OperationItemSelector::ItemIdU16 { raw_offset: 16 },
    },
    RecoveredOperationItemAssociation {
        class_name: "GopRocket",
        item_id: 33,
        symbol: "guideRocket",
        selector: OperationItemSelector::ItemIdU16 { raw_offset: 16 },
    },
    RecoveredOperationItemAssociation {
        class_name: "GopStraightRocket",
        item_id: 73,
        symbol: "straightRocket",
        selector: OperationItemSelector::FixedClass,
    },
    RecoveredOperationItemAssociation {
        class_name: "GopTimebomb",
        item_id: 13,
        symbol: "timeBomb",
        selector: OperationItemSelector::FixedClass,
    },
    RecoveredOperationItemAssociation {
        class_name: "GopBigTimebomb",
        item_id: 122,
        symbol: "bigTimeBomb",
        selector: OperationItemSelector::FixedClass,
    },
    RecoveredOperationItemAssociation {
        class_name: "GopSnowWaterfly",
        item_id: 118,
        symbol: "snowWaterFly",
        selector: OperationItemSelector::FixedClass,
    },
    RecoveredOperationItemAssociation {
        class_name: "GopIcefly",
        item_id: 80,
        symbol: "iceFly",
        selector: OperationItemSelector::FixedClass,
    },
    RecoveredOperationItemAssociation {
        class_name: "GopShield",
        item_id: 10,
        symbol: "shield",
        selector: OperationItemSelector::ItemIdU16 { raw_offset: 16 },
    },
    RecoveredOperationItemAssociation {
        class_name: "GopShield",
        item_id: 18,
        symbol: "superShield",
        selector: OperationItemSelector::ItemIdU16 { raw_offset: 16 },
    },
    RecoveredOperationItemAssociation {
        class_name: "GopShield",
        item_id: 103,
        symbol: "superMagnet",
        selector: OperationItemSelector::ItemIdU16 { raw_offset: 16 },
    },
    RecoveredOperationItemAssociation {
        class_name: "GopSpecialShield",
        item_id: 40,
        symbol: "specialShield",
        selector: OperationItemSelector::FixedClass,
    },
    RecoveredOperationItemAssociation {
        class_name: "GopCloud",
        item_id: 0,
        symbol: "cloud",
        selector: OperationItemSelector::VariantByte {
            raw_offset: 24,
            value: 0,
        },
    },
    RecoveredOperationItemAssociation {
        class_name: "GopCloud",
        item_id: 1,
        symbol: "darkCloud",
        selector: OperationItemSelector::VariantByte {
            raw_offset: 24,
            value: 3,
        },
    },
    RecoveredOperationItemAssociation {
        class_name: "GopCloud",
        item_id: 43,
        symbol: "rainbowCloud",
        selector: OperationItemSelector::VariantByte {
            raw_offset: 24,
            value: 6,
        },
    },
    RecoveredOperationItemAssociation {
        class_name: "GopCloud2",
        item_id: 114,
        symbol: "cloud2",
        selector: OperationItemSelector::VariantByte {
            raw_offset: 24,
            value: 0,
        },
    },
    RecoveredOperationItemAssociation {
        class_name: "GopCloud2",
        item_id: 115,
        symbol: "darkCloud2",
        selector: OperationItemSelector::VariantByte {
            raw_offset: 24,
            value: 3,
        },
    },
    RecoveredOperationItemAssociation {
        class_name: "GopCloud2",
        item_id: 116,
        symbol: "rainbowCloud2",
        selector: OperationItemSelector::VariantByte {
            raw_offset: 24,
            value: 6,
        },
    },
];

/// Complete heading-level coverage of the supplied page. The order follows
/// the page so missing or duplicated entries remain easy to audit.
pub static P5136_GAMEPLAY_ITEM_HINTS: &[GameplayItemHint] = &[
    GameplayItemHint {
        slug: "booster",
        korean_name: "부스터",
        category: GameplayItemCategory::Acceleration,
        effect_summary_ko: "자신의 카트를 잠시 가속한다.",
        targets: &[GameplayTargetScope::SelfKart],
        effects: &[GameplayEffectHint::SpeedBoost],
        item_symbols: &[ItemSymbolLink {
            item_id: 6,
            symbol: "booster",
            evidence: FALLBACK,
        }],
        operation_links: &[],
    },
    GameplayItemHint {
        slug: "power_booster",
        korean_name: "파워 부스터",
        category: GameplayItemCategory::Acceleration,
        effect_summary_ko: "팀 게이지가 차면 일반 부스터에서 변환되는 장시간 부스터다.",
        targets: &[GameplayTargetScope::SelfKart],
        effects: &[GameplayEffectHint::SpeedBoost],
        item_symbols: &[],
        operation_links: &[],
    },
    GameplayItemHint {
        slug: "siren",
        korean_name: "사이렌",
        category: GameplayItemCategory::Acceleration,
        effect_summary_ko: "사용자를 가속하고 접촉한 카트를 회전시킨다.",
        targets: &[
            GameplayTargetScope::SelfKart,
            GameplayTargetScope::ContactedKart,
        ],
        effects: &[
            GameplayEffectHint::SpeedBoost,
            GameplayEffectHint::ContactSpin,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 24,
            symbol: "siren",
            evidence: SUPPLEMENT,
        }],
        operation_links: &[OperationLink {
            class_name: "GopSiren",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "zongzi",
        korean_name: "쭝쯔",
        category: GameplayItemCategory::Acceleration,
        effect_summary_ko: "이벤트 모드에서 질주하며 접촉 상대를 밟는다.",
        targets: &[
            GameplayTargetScope::SelfKart,
            GameplayTargetScope::ContactedKart,
        ],
        effects: &[
            GameplayEffectHint::SpeedBoost,
            GameplayEffectHint::CrushKart,
        ],
        item_symbols: &[],
        operation_links: &[],
    },
    GameplayItemHint {
        slug: "magnet",
        korean_name: "자석",
        category: GameplayItemCategory::Acceleration,
        effect_summary_ko: "조준한 앞선 카트 쪽으로 사용자를 빠르게 끌어간다.",
        targets: &[
            GameplayTargetScope::SelfKart,
            GameplayTargetScope::AimedKart,
        ],
        effects: &[GameplayEffectHint::PullTowardTarget],
        item_symbols: &[ItemSymbolLink {
            item_id: 5,
            symbol: "magnet",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopMagnet",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "golden_magnet",
        korean_name: "황금 자석",
        category: GameplayItemCategory::Acceleration,
        effect_summary_ko: "자석 가속과 자기 방어를 동시에 적용한다.",
        targets: &[
            GameplayTargetScope::SelfKart,
            GameplayTargetScope::AimedKart,
        ],
        effects: &[
            GameplayEffectHint::PullTowardTarget,
            GameplayEffectHint::TemporaryShield,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 103,
            symbol: "superMagnet",
            evidence: SUPPLEMENT,
        }],
        operation_links: &[OperationLink {
            class_name: "GopSuperMag",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "missile",
        korean_name: "미사일",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "조준한 카트를 추적해 공중으로 띄운다.",
        targets: &[GameplayTargetScope::AimedKart],
        effects: &[GameplayEffectHint::LaunchAirborne],
        item_symbols: &[ItemSymbolLink {
            item_id: 7,
            symbol: "rocket",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopRocket",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "first_place_missile",
        korean_name: "1등 미사일",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "현재 1위를 자동 추적해 미사일 효과를 준다.",
        targets: &[GameplayTargetScope::FirstPlaceKart],
        effects: &[GameplayEffectHint::LaunchAirborne],
        item_symbols: &[ItemSymbolLink {
            item_id: 33,
            symbol: "guideRocket",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopRocket",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "rocket_launcher",
        korean_name: "로켓포",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "플래그전에서 여러 발의 조준 미사일을 제공한다.",
        targets: &[GameplayTargetScope::AimedKart],
        effects: &[GameplayEffectHint::LaunchAirborne],
        item_symbols: &[],
        operation_links: &[OperationLink {
            class_name: "GopRocket",
            evidence: NAMED,
        }],
    },
    GameplayItemHint {
        slug: "golden_missile",
        korean_name: "황금 미사일",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "풍선 방어를 무시하고 조준한 카트를 띄운다.",
        targets: &[GameplayTargetScope::AimedKart],
        effects: &[GameplayEffectHint::LaunchAirborne],
        item_symbols: &[ItemSymbolLink {
            item_id: 32,
            symbol: "goldRocket",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopGoldRocket",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "tiger_missile",
        korean_name: "호랑이 미사일",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "조준한 카트의 속도와 시야를 방해한다.",
        targets: &[GameplayTargetScope::AimedKart],
        effects: &[
            GameplayEffectHint::SlowKart,
            GameplayEffectHint::ObscureVision,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 99,
            symbol: "tigerRocket",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopTigerRocket",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "electromagnetic_missile",
        korean_name: "전자기 미사일",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "표적을 전자기장에 가두고 탈출 시 주변 카트를 감속한다.",
        targets: &[
            GameplayTargetScope::AimedKart,
            GameplayTargetScope::NearbyOtherKarts,
        ],
        effects: &[
            GameplayEffectHint::ElectromagneticField,
            GameplayEffectHint::SlowKart,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 104,
            symbol: "lockdownRocket",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopLockdownRocket",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "snow_fairy",
        korean_name: "눈의 요정",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "표적을 작게 만들고 감속한다.",
        targets: &[GameplayTargetScope::AimedKart],
        effects: &[GameplayEffectHint::ShrinkKart, GameplayEffectHint::SlowKart],
        item_symbols: &[ItemSymbolLink {
            item_id: 112,
            symbol: "snowman",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopSnowman",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "random_missile",
        korean_name: "랜덤 미사일",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "앞선 상대 팀원 한 명을 무작위로 자동 공격한다.",
        targets: &[GameplayTargetScope::RandomAheadOpponent],
        effects: &[GameplayEffectHint::LaunchAirborne],
        item_symbols: &[],
        operation_links: &[],
    },
    GameplayItemHint {
        slug: "waterbomb",
        korean_name: "물폭탄",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "전방 일정 구역을 폭발시켜 범위 내 카트를 물에 가둔다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[GameplayEffectHint::TrapInWater],
        item_symbols: &[ItemSymbolLink {
            item_id: 9,
            symbol: "waterBomb",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopWaterbomb",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "timed_waterbomb",
        korean_name: "자폭(시한) 물폭탄",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "사용자 위치를 중심으로 지연 폭발하며 사용자도 맞는다.",
        targets: &[
            GameplayTargetScope::SelfKart,
            GameplayTargetScope::TrackArea,
        ],
        effects: &[GameplayEffectHint::TrapInWater],
        item_symbols: &[ItemSymbolLink {
            item_id: 13,
            symbol: "timeBomb",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopTimebomb",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "infected_waterbomb",
        korean_name: "독성 물폭탄",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "범위 내 카트를 물에 가둔 뒤 아이템 슬롯도 잠근다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[
            GameplayEffectHint::TrapInWater,
            GameplayEffectHint::LockItemSlots,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 27,
            symbol: "infectedBomb",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopInfectedBomb",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "coke_bomb",
        korean_name: "코-크 폭탄",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "전방 범위의 카트를 콜라 물기둥에 가둔다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[GameplayEffectHint::TrapInWater],
        item_symbols: &[ItemSymbolLink {
            item_id: 20,
            symbol: "cokeBomb",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopCokebomb",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "ice_bomb",
        korean_name: "얼음폭탄",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "전방 범위의 카트를 얼음에 가둔다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[GameplayEffectHint::TrapInWater],
        item_symbols: &[ItemSymbolLink {
            item_id: 34,
            symbol: "snowBomb",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopSnowbomb",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "rolling_waterbomb",
        korean_name: "롤링 물폭탄",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "트랙을 구르다가 충돌 또는 시간 경과로 물폭탄을 만든다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[
            GameplayEffectHint::PlaceRollingBomb,
            GameplayEffectHint::TrapInWater,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 22,
            symbol: "rollingCokeBomb",
            evidence: EXECUTABLE,
        }],
        operation_links: &[
            OperationLink {
                class_name: "GopRollingbomb",
                evidence: AMBIGUOUS,
            },
            OperationLink {
                class_name: "GopRollingCokebomb",
                evidence: AMBIGUOUS,
            },
        ],
    },
    GameplayItemHint {
        slug: "net",
        korean_name: "그물",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "전방 구역의 카트를 그물에 가둬 움직이지 못하게 한다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[GameplayEffectHint::Immobilize],
        item_symbols: &[],
        operation_links: &[],
    },
    GameplayItemHint {
        slug: "waterfly",
        korean_name: "물파리",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "바로 앞 순위 카트를 자동 추적해 물에 가둔다.",
        targets: &[GameplayTargetScope::NearestAheadOpponent],
        effects: &[GameplayEffectHint::TrapInWater],
        item_symbols: &[ItemSymbolLink {
            item_id: 4,
            symbol: "waterFly",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopWaterfly",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "ice_waterfly",
        korean_name: "얼음 물파리",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "앞선 카트를 자동 추적해 더 강한 얼음 구속을 건다.",
        targets: &[GameplayTargetScope::NearestAheadOpponent],
        effects: &[GameplayEffectHint::TrapInWater],
        item_symbols: &[ItemSymbolLink {
            item_id: 118,
            symbol: "snowWaterFly",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopSnowWaterfly",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "infected_waterfly",
        korean_name: "독성 물파리",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "앞선 카트를 물에 가둔 뒤 아이템 슬롯도 잠근다.",
        targets: &[GameplayTargetScope::NearestAheadOpponent],
        effects: &[
            GameplayEffectHint::TrapInWater,
            GameplayEffectHint::LockItemSlots,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 119,
            symbol: "infectedWaterFly",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopInfectedWaterfly",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "bomb_waterfly",
        korean_name: "폭탄 물파리",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "바로 앞 카트에 시한 물폭탄을 붙여 잠시 뒤 주변까지 물에 가둔다.",
        targets: &[
            GameplayTargetScope::NearestAheadOpponent,
            GameplayTargetScope::NearbyKartsIncludingSource,
        ],
        effects: &[
            GameplayEffectHint::DelayedAreaWaterBomb,
            GameplayEffectHint::TrapInWater,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 120,
            symbol: "waterbombFly",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopWaterbombFly",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "ufo",
        korean_name: "우주선",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "현재 1위를 자동 추적해 일정 시간 감속한다.",
        targets: &[GameplayTargetScope::FirstPlaceKart],
        effects: &[GameplayEffectHint::UfoSlow],
        item_symbols: &[ItemSymbolLink {
            item_id: 3,
            symbol: "ufo",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopUfo",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "ufo_carrier",
        korean_name: "우주모함",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "1위를 우주선으로 감속하고 그 주변 카트에도 감속을 건다.",
        targets: &[
            GameplayTargetScope::FirstPlaceKart,
            GameplayTargetScope::NearbyOtherKarts,
        ],
        effects: &[GameplayEffectHint::UfoSlow, GameplayEffectHint::SlowKart],
        item_symbols: &[],
        operation_links: &[OperationLink {
            class_name: "GopAreaUfo",
            evidence: NAMED,
        }],
    },
    GameplayItemHint {
        slug: "thunderbolt",
        korean_name: "벼락",
        category: GameplayItemCategory::Attack,
        effect_summary_ko: "자신보다 앞선 모든 상대를 동시에 공격한다.",
        targets: &[GameplayTargetScope::AheadOpponents],
        effects: &[GameplayEffectHint::LightningStrike],
        item_symbols: &[ItemSymbolLink {
            item_id: 111,
            symbol: "thunderbolt",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopThunderbolt",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "shield",
        korean_name: "실드",
        category: GameplayItemCategory::Defense,
        effect_summary_ko: "자신에게 들어오는 방어 가능한 공격 한 번을 막는다.",
        targets: &[GameplayTargetScope::SelfKart],
        effects: &[GameplayEffectHint::TemporaryShield],
        item_symbols: &[ItemSymbolLink {
            item_id: 10,
            symbol: "shield",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopShield",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "angel",
        korean_name: "천사",
        category: GameplayItemCategory::Defense,
        effect_summary_ko: "자신과 같은 팀 전체에 방어 효과를 적용한다.",
        targets: &[GameplayTargetScope::OwnTeam],
        effects: &[GameplayEffectHint::TemporaryShield],
        item_symbols: &[ItemSymbolLink {
            item_id: 11,
            symbol: "angel",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopAngel",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "super_shield",
        korean_name: "슈퍼 실드",
        category: GameplayItemCategory::Defense,
        effect_summary_ko: "자신을 보호하면서 카트 속도도 높이는 강화 실드다.",
        targets: &[GameplayTargetScope::SelfKart],
        effects: &[
            GameplayEffectHint::TemporaryShield,
            GameplayEffectHint::SpeedBoost,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 18,
            symbol: "superShield",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopShield",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "golden_shield",
        korean_name: "황금 실드",
        category: GameplayItemCategory::Defense,
        effect_summary_ko: "지속 시간 동안 방어 가능한 공격을 반복해서 막는다.",
        targets: &[GameplayTargetScope::SelfKart],
        effects: &[GameplayEffectHint::TemporaryShield],
        item_symbols: &[ItemSymbolLink {
            item_id: 36,
            symbol: "goldShield",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopGoldShield",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "protect_shield",
        korean_name: "프로텍트 실드",
        category: GameplayItemCategory::Defense,
        effect_summary_ko: "지속 시간 동안 방어 가능한 공격을 반복해서 막는 특수 실드다.",
        targets: &[GameplayTargetScope::SelfKart],
        effects: &[GameplayEffectHint::TemporaryShield],
        item_symbols: &[ItemSymbolLink {
            item_id: 81,
            symbol: "protectShield",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopGoldShield",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "siren_shield",
        korean_name: "사이렌 실드",
        category: GameplayItemCategory::Defense,
        effect_summary_ko: "사이렌 접촉 공격과 자기 방어를 함께 제공한다.",
        targets: &[
            GameplayTargetScope::SelfKart,
            GameplayTargetScope::ContactedKart,
        ],
        effects: &[
            GameplayEffectHint::ContactSpin,
            GameplayEffectHint::TemporaryShield,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 106,
            symbol: "sirenShield",
            evidence: EXECUTABLE,
        }],
        operation_links: &[
            OperationLink {
                class_name: "GopSirenShield",
                evidence: VERIFIED,
            },
            OperationLink {
                class_name: "GopGoldShield",
                evidence: VERIFIED,
            },
        ],
    },
    GameplayItemHint {
        slug: "emp",
        korean_name: "전자파",
        category: GameplayItemCategory::Defense,
        effect_summary_ko: "자신 또는 팀에 걸린 우주선 효과를 제거한다.",
        targets: &[GameplayTargetScope::SelfKart, GameplayTargetScope::OwnTeam],
        effects: &[GameplayEffectHint::RemoveUfo],
        item_symbols: &[ItemSymbolLink {
            item_id: 12,
            symbol: "emp",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopEmp",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "cloud",
        korean_name: "먹구름",
        category: GameplayItemCategory::Placement,
        effect_summary_ko: "뒤쪽 트랙에 통과자의 시야를 가리는 구름을 둔다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[GameplayEffectHint::PlaceVisionCloud],
        item_symbols: &[ItemSymbolLink {
            item_id: 1,
            symbol: "darkCloud",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopCloud",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "ink_cloud",
        korean_name: "먹물구름",
        category: GameplayItemCategory::Placement,
        effect_summary_ko: "고글로 지속시간을 줄이기 어려운 강화 시야 구름이다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[
            GameplayEffectHint::PlaceVisionCloud,
            GameplayEffectHint::ObscureVision,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 115,
            symbol: "darkCloud2",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopCloud2",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "new_cloud",
        korean_name: "NEW 구름",
        category: GameplayItemCategory::Placement,
        effect_summary_ko: "한 명이 지나가도 유지되는 무지개 시야 구름이다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[
            GameplayEffectHint::PlaceVisionCloud,
            GameplayEffectHint::ObscureVision,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 114,
            symbol: "cloud2",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopCloud2",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "fairy_cloud",
        korean_name: "요정 구름",
        category: GameplayItemCategory::Placement,
        effect_summary_ko: "고글을 무시하는 NEW 구름 계열의 테마 변형이다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[
            GameplayEffectHint::PlaceVisionCloud,
            GameplayEffectHint::ObscureVision,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 43,
            symbol: "rainbowCloud",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopCloud",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "banana",
        korean_name: "바나나",
        category: GameplayItemCategory::Placement,
        effect_summary_ko: "밟은 카트를 회전시키는 바닥 트랩을 설치한다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[GameplayEffectHint::SpinKart],
        item_symbols: &[ItemSymbolLink {
            item_id: 8,
            symbol: "banana",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopBanana",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "big_banana",
        korean_name: "대왕 바나나",
        category: GameplayItemCategory::Placement,
        effect_summary_ko: "더 큰 판정의 바나나 트랩을 설치한다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[GameplayEffectHint::SpinKart],
        item_symbols: &[ItemSymbolLink {
            item_id: 85,
            symbol: "bigBanana",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopBanana",
            evidence: NAMED,
        }],
    },
    GameplayItemHint {
        slug: "mine",
        korean_name: "지뢰",
        category: GameplayItemCategory::Placement,
        effect_summary_ko: "밟은 카트를 공중으로 띄우는 지뢰를 설치한다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[
            GameplayEffectHint::PlaceMine,
            GameplayEffectHint::LaunchAirborne,
        ],
        item_symbols: &[],
        operation_links: &[OperationLink {
            class_name: "GopMine",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "duck_bomb",
        korean_name: "오리폭탄",
        category: GameplayItemCategory::Placement,
        effect_summary_ko: "밟은 카트를 띄우는 오리 모양 지뢰 변형이다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[
            GameplayEffectHint::PlaceMine,
            GameplayEffectHint::LaunchAirborne,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 45,
            symbol: "duckMine",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopMine",
            evidence: NAMED,
        }],
    },
    GameplayItemHint {
        slug: "water_mine",
        korean_name: "물지뢰",
        category: GameplayItemCategory::Placement,
        effect_summary_ko: "밟은 카트를 물에 가두는 지뢰를 설치한다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[
            GameplayEffectHint::PlaceMine,
            GameplayEffectHint::TrapInWater,
        ],
        item_symbols: &[ItemSymbolLink {
            item_id: 37,
            symbol: "waterMine",
            evidence: EXECUTABLE,
        }],
        operation_links: &[OperationLink {
            class_name: "GopWaterMine",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "booby_trap",
        korean_name: "부비트랩",
        category: GameplayItemCategory::Placement,
        effect_summary_ko: "밟은 카트를 진행 반대 방향으로 밀어내는 트랩이다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[GameplayEffectHint::Knockback],
        item_symbols: &[],
        operation_links: &[OperationLink {
            class_name: "GopForceZone",
            evidence: NAMED,
        }],
    },
    GameplayItemHint {
        slug: "barricade",
        korean_name: "바리케이드",
        category: GameplayItemCategory::Placement,
        effect_summary_ko: "1위 전방에 충돌 장애물 여러 개를 설치한다.",
        targets: &[
            GameplayTargetScope::FirstPlaceKart,
            GameplayTargetScope::TrackArea,
        ],
        effects: &[GameplayEffectHint::Knockback],
        item_symbols: &[ItemSymbolLink {
            item_id: 113,
            symbol: "barricade",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopBarricade",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "devil",
        korean_name: "대마왕",
        category: GameplayItemCategory::Status,
        effect_summary_ko: "사용자를 제외한 참가자의 좌우 조향을 반전한다.",
        targets: &[GameplayTargetScope::EveryoneExceptSource],
        effects: &[GameplayEffectHint::ReverseSteering],
        item_symbols: &[ItemSymbolLink {
            item_id: 2,
            symbol: "devil",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopDevil",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "doctor_r",
        korean_name: "닥터 R",
        category: GameplayItemCategory::Status,
        effect_summary_ko: "리버스 트랙에서 상대의 상하좌우 입력을 반전한다.",
        targets: &[GameplayTargetScope::NonAlliedKarts],
        effects: &[
            GameplayEffectHint::ReverseSteering,
            GameplayEffectHint::ReverseThrottle,
        ],
        item_symbols: &[],
        operation_links: &[OperationLink {
            class_name: "GopDrmad",
            evidence: NAMED,
        }],
    },
    GameplayItemHint {
        slug: "jiangshi",
        korean_name: "강시",
        category: GameplayItemCategory::Status,
        effect_summary_ko: "사용자를 제외한 참가자의 전진·후진 입력을 반전한다.",
        targets: &[GameplayTargetScope::EveryoneExceptSource],
        effects: &[GameplayEffectHint::ReverseThrottle],
        item_symbols: &[],
        operation_links: &[
            OperationLink {
                class_name: "GopMqDevil",
                evidence: AMBIGUOUS,
            },
            OperationLink {
                class_name: "GopNewDevil",
                evidence: AMBIGUOUS,
            },
        ],
    },
    GameplayItemHint {
        slug: "first_place_devil",
        korean_name: "1위 대마왕",
        category: GameplayItemCategory::Status,
        effect_summary_ko: "현재 1위 한 명의 좌우 조향을 반전한다.",
        targets: &[GameplayTargetScope::FirstPlaceKart],
        effects: &[GameplayEffectHint::ReverseSteering],
        item_symbols: &[],
        operation_links: &[
            OperationLink {
                class_name: "GopMqDevil",
                evidence: AMBIGUOUS,
            },
            OperationLink {
                class_name: "GopNewDevil",
                evidence: AMBIGUOUS,
            },
        ],
    },
    GameplayItemHint {
        slug: "slot_lock",
        korean_name: "자물쇠",
        category: GameplayItemCategory::Status,
        effect_summary_ko: "상대 팀 전원의 아이템 슬롯 사용을 잠시 막는다.",
        targets: &[GameplayTargetScope::OpposingTeam],
        effects: &[GameplayEffectHint::LockItemSlots],
        item_symbols: &[ItemSymbolLink {
            item_id: 110,
            symbol: "slotLock",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopSlotLock",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "scanning",
        korean_name: "스캐닝",
        category: GameplayItemCategory::Utility,
        effect_summary_ko: "자신의 팀이 상대 팀의 현재 아이템 슬롯을 보게 한다.",
        targets: &[
            GameplayTargetScope::OwnTeam,
            GameplayTargetScope::OpposingTeam,
        ],
        effects: &[GameplayEffectHint::RevealItemSlots],
        item_symbols: &[ItemSymbolLink {
            item_id: 109,
            symbol: "scanning",
            evidence: FALLBACK,
        }],
        operation_links: &[OperationLink {
            class_name: "GopScanning",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "ghost",
        korean_name: "고스트",
        category: GameplayItemCategory::Utility,
        effect_summary_ko: "사용자를 상대 시야에서 숨기되 충돌과 공격 판정은 유지한다.",
        targets: &[GameplayTargetScope::SelfKart],
        effects: &[GameplayEffectHint::HideKart],
        item_symbols: &[],
        operation_links: &[OperationLink {
            class_name: "GopGhost",
            evidence: VERIFIED,
        }],
    },
    GameplayItemHint {
        slug: "oil",
        korean_name: "검은 기름",
        category: GameplayItemCategory::Utility,
        effect_summary_ko: "밟은 카트의 시야를 가리는 바닥 기름을 설치한다.",
        targets: &[GameplayTargetScope::TrackArea],
        effects: &[GameplayEffectHint::OilBlind],
        item_symbols: &[],
        operation_links: &[OperationLink {
            class_name: "GopOil",
            evidence: VERIFIED,
        }],
    },
];

#[must_use]
pub fn gameplay_item_by_slug(slug: &str) -> Option<&'static GameplayItemHint> {
    P5136_GAMEPLAY_ITEM_HINTS
        .iter()
        .find(|item| item.slug == slug)
}

#[must_use]
pub fn gameplay_item_by_symbol(symbol: &str) -> Option<&'static GameplayItemHint> {
    P5136_GAMEPLAY_ITEM_HINTS.iter().find(|item| {
        item.item_symbols
            .iter()
            .any(|link| link.symbol.eq_ignore_ascii_case(symbol))
    })
}

#[must_use]
pub fn gameplay_item_by_id(item_id: i16) -> Option<&'static GameplayItemHint> {
    P5136_GAMEPLAY_ITEM_HINTS
        .iter()
        .find(|item| item.item_symbols.iter().any(|link| link.item_id == item_id))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn supplied_page_has_exactly_54_unique_heading_entries() {
        const EXPECTED_PAGE_ORDER: [&str; 54] = [
            "부스터",
            "파워 부스터",
            "사이렌",
            "쭝쯔",
            "자석",
            "황금 자석",
            "미사일",
            "1등 미사일",
            "로켓포",
            "황금 미사일",
            "호랑이 미사일",
            "전자기 미사일",
            "눈의 요정",
            "랜덤 미사일",
            "물폭탄",
            "자폭(시한) 물폭탄",
            "독성 물폭탄",
            "코-크 폭탄",
            "얼음폭탄",
            "롤링 물폭탄",
            "그물",
            "물파리",
            "얼음 물파리",
            "독성 물파리",
            "폭탄 물파리",
            "우주선",
            "우주모함",
            "벼락",
            "실드",
            "천사",
            "슈퍼 실드",
            "황금 실드",
            "프로텍트 실드",
            "사이렌 실드",
            "전자파",
            "먹구름",
            "먹물구름",
            "NEW 구름",
            "요정 구름",
            "바나나",
            "대왕 바나나",
            "지뢰",
            "오리폭탄",
            "물지뢰",
            "부비트랩",
            "바리케이드",
            "대마왕",
            "닥터 R",
            "강시",
            "1위 대마왕",
            "자물쇠",
            "스캐닝",
            "고스트",
            "검은 기름",
        ];
        assert_eq!(P5136_GAMEPLAY_ITEM_HINTS.len(), 54);
        assert_eq!(
            P5136_GAMEPLAY_ITEM_HINTS
                .iter()
                .map(|item| item.korean_name)
                .collect::<Vec<_>>(),
            EXPECTED_PAGE_ORDER
        );
        let slugs = P5136_GAMEPLAY_ITEM_HINTS
            .iter()
            .map(|item| item.slug)
            .collect::<HashSet<_>>();
        let names = P5136_GAMEPLAY_ITEM_HINTS
            .iter()
            .map(|item| item.korean_name)
            .collect::<HashSet<_>>();
        assert_eq!(slugs.len(), 54);
        assert_eq!(names.len(), 54);
    }

    #[test]
    fn only_explicitly_deferred_headings_retain_ambiguous_links() {
        let deferred = P5136_GAMEPLAY_ITEM_HINTS
            .iter()
            .filter(|item| {
                item.reverse_engineering_scope() == ReverseEngineeringScope::DeferredByUser
            })
            .map(|item| item.slug)
            .collect::<Vec<_>>();
        assert_eq!(
            deferred,
            ["rolling_waterbomb", "jiangshi", "first_place_devil"]
        );

        for item in P5136_GAMEPLAY_ITEM_HINTS
            .iter()
            .filter(|item| item.reverse_engineering_scope() == ReverseEngineeringScope::InScope)
        {
            assert!(
                item.operation_links
                    .iter()
                    .all(|link| link.evidence != OperationLinkEvidence::AmbiguousCandidate),
                "{} still has an in-scope ambiguous operation link",
                item.slug
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the independent literal association oracle keeps all recovered selectors visible"
    )]
    fn recovered_operation_item_joins_are_literal_and_unique() {
        let actual = P5136_RECOVERED_OPERATION_ITEM_ASSOCIATIONS
            .iter()
            .map(|link| (link.class_name, link.item_id, link.symbol, link.selector))
            .collect::<Vec<_>>();
        assert_eq!(actual.len(), 17);
        assert_eq!(
            actual,
            vec![
                (
                    "GopRocket",
                    7,
                    "rocket",
                    OperationItemSelector::ItemIdU16 { raw_offset: 16 }
                ),
                (
                    "GopRocket",
                    33,
                    "guideRocket",
                    OperationItemSelector::ItemIdU16 { raw_offset: 16 }
                ),
                (
                    "GopStraightRocket",
                    73,
                    "straightRocket",
                    OperationItemSelector::FixedClass
                ),
                (
                    "GopTimebomb",
                    13,
                    "timeBomb",
                    OperationItemSelector::FixedClass
                ),
                (
                    "GopBigTimebomb",
                    122,
                    "bigTimeBomb",
                    OperationItemSelector::FixedClass
                ),
                (
                    "GopSnowWaterfly",
                    118,
                    "snowWaterFly",
                    OperationItemSelector::FixedClass
                ),
                ("GopIcefly", 80, "iceFly", OperationItemSelector::FixedClass),
                (
                    "GopShield",
                    10,
                    "shield",
                    OperationItemSelector::ItemIdU16 { raw_offset: 16 }
                ),
                (
                    "GopShield",
                    18,
                    "superShield",
                    OperationItemSelector::ItemIdU16 { raw_offset: 16 }
                ),
                (
                    "GopShield",
                    103,
                    "superMagnet",
                    OperationItemSelector::ItemIdU16 { raw_offset: 16 }
                ),
                (
                    "GopSpecialShield",
                    40,
                    "specialShield",
                    OperationItemSelector::FixedClass
                ),
                (
                    "GopCloud",
                    0,
                    "cloud",
                    OperationItemSelector::VariantByte {
                        raw_offset: 24,
                        value: 0
                    }
                ),
                (
                    "GopCloud",
                    1,
                    "darkCloud",
                    OperationItemSelector::VariantByte {
                        raw_offset: 24,
                        value: 3
                    }
                ),
                (
                    "GopCloud",
                    43,
                    "rainbowCloud",
                    OperationItemSelector::VariantByte {
                        raw_offset: 24,
                        value: 6
                    }
                ),
                (
                    "GopCloud2",
                    114,
                    "cloud2",
                    OperationItemSelector::VariantByte {
                        raw_offset: 24,
                        value: 0
                    }
                ),
                (
                    "GopCloud2",
                    115,
                    "darkCloud2",
                    OperationItemSelector::VariantByte {
                        raw_offset: 24,
                        value: 3
                    }
                ),
                (
                    "GopCloud2",
                    116,
                    "rainbowCloud2",
                    OperationItemSelector::VariantByte {
                        raw_offset: 24,
                        value: 6
                    }
                ),
            ]
        );

        let unique = actual
            .iter()
            .map(|(_, item_id, symbol, _)| (*item_id, *symbol))
            .collect::<HashSet<_>>();
        assert_eq!(unique.len(), actual.len());
    }

    // An exhaustive literal oracle is intentionally kept independent of the
    // production table instead of generated from it.
    #[allow(clippy::too_many_lines)]
    #[test]
    fn every_page_heading_has_golden_category_target_and_effect_semantics() {
        use GameplayEffectHint::*;
        use GameplayItemCategory::*;
        use GameplayTargetScope::*;

        type SemanticGolden = (
            &'static str,
            GameplayItemCategory,
            &'static [GameplayTargetScope],
            &'static [GameplayEffectHint],
        );
        const EXPECTED: [SemanticGolden; 54] = [
            ("booster", Acceleration, &[SelfKart], &[SpeedBoost]),
            ("power_booster", Acceleration, &[SelfKart], &[SpeedBoost]),
            (
                "siren",
                Acceleration,
                &[SelfKart, ContactedKart],
                &[SpeedBoost, ContactSpin],
            ),
            (
                "zongzi",
                Acceleration,
                &[SelfKart, ContactedKart],
                &[SpeedBoost, CrushKart],
            ),
            (
                "magnet",
                Acceleration,
                &[SelfKart, AimedKart],
                &[PullTowardTarget],
            ),
            (
                "golden_magnet",
                Acceleration,
                &[SelfKart, AimedKart],
                &[PullTowardTarget, TemporaryShield],
            ),
            ("missile", Attack, &[AimedKart], &[LaunchAirborne]),
            (
                "first_place_missile",
                Attack,
                &[FirstPlaceKart],
                &[LaunchAirborne],
            ),
            ("rocket_launcher", Attack, &[AimedKart], &[LaunchAirborne]),
            ("golden_missile", Attack, &[AimedKart], &[LaunchAirborne]),
            (
                "tiger_missile",
                Attack,
                &[AimedKart],
                &[SlowKart, ObscureVision],
            ),
            (
                "electromagnetic_missile",
                Attack,
                &[AimedKart, NearbyOtherKarts],
                &[ElectromagneticField, SlowKart],
            ),
            ("snow_fairy", Attack, &[AimedKart], &[ShrinkKart, SlowKart]),
            (
                "random_missile",
                Attack,
                &[RandomAheadOpponent],
                &[LaunchAirborne],
            ),
            ("waterbomb", Attack, &[TrackArea], &[TrapInWater]),
            (
                "timed_waterbomb",
                Attack,
                &[SelfKart, TrackArea],
                &[TrapInWater],
            ),
            (
                "infected_waterbomb",
                Attack,
                &[TrackArea],
                &[TrapInWater, LockItemSlots],
            ),
            ("coke_bomb", Attack, &[TrackArea], &[TrapInWater]),
            ("ice_bomb", Attack, &[TrackArea], &[TrapInWater]),
            (
                "rolling_waterbomb",
                Attack,
                &[TrackArea],
                &[PlaceRollingBomb, TrapInWater],
            ),
            ("net", Attack, &[TrackArea], &[Immobilize]),
            ("waterfly", Attack, &[NearestAheadOpponent], &[TrapInWater]),
            (
                "ice_waterfly",
                Attack,
                &[NearestAheadOpponent],
                &[TrapInWater],
            ),
            (
                "infected_waterfly",
                Attack,
                &[NearestAheadOpponent],
                &[TrapInWater, LockItemSlots],
            ),
            (
                "bomb_waterfly",
                Attack,
                &[NearestAheadOpponent, NearbyKartsIncludingSource],
                &[DelayedAreaWaterBomb, TrapInWater],
            ),
            ("ufo", Attack, &[FirstPlaceKart], &[UfoSlow]),
            (
                "ufo_carrier",
                Attack,
                &[FirstPlaceKart, NearbyOtherKarts],
                &[UfoSlow, SlowKart],
            ),
            ("thunderbolt", Attack, &[AheadOpponents], &[LightningStrike]),
            ("shield", Defense, &[SelfKart], &[TemporaryShield]),
            ("angel", Defense, &[OwnTeam], &[TemporaryShield]),
            (
                "super_shield",
                Defense,
                &[SelfKart],
                &[TemporaryShield, SpeedBoost],
            ),
            ("golden_shield", Defense, &[SelfKart], &[TemporaryShield]),
            ("protect_shield", Defense, &[SelfKart], &[TemporaryShield]),
            (
                "siren_shield",
                Defense,
                &[SelfKart, ContactedKart],
                &[ContactSpin, TemporaryShield],
            ),
            ("emp", Defense, &[SelfKart, OwnTeam], &[RemoveUfo]),
            ("cloud", Placement, &[TrackArea], &[PlaceVisionCloud]),
            (
                "ink_cloud",
                Placement,
                &[TrackArea],
                &[PlaceVisionCloud, ObscureVision],
            ),
            (
                "new_cloud",
                Placement,
                &[TrackArea],
                &[PlaceVisionCloud, ObscureVision],
            ),
            (
                "fairy_cloud",
                Placement,
                &[TrackArea],
                &[PlaceVisionCloud, ObscureVision],
            ),
            ("banana", Placement, &[TrackArea], &[SpinKart]),
            ("big_banana", Placement, &[TrackArea], &[SpinKart]),
            (
                "mine",
                Placement,
                &[TrackArea],
                &[PlaceMine, LaunchAirborne],
            ),
            (
                "duck_bomb",
                Placement,
                &[TrackArea],
                &[PlaceMine, LaunchAirborne],
            ),
            (
                "water_mine",
                Placement,
                &[TrackArea],
                &[PlaceMine, TrapInWater],
            ),
            ("booby_trap", Placement, &[TrackArea], &[Knockback]),
            (
                "barricade",
                Placement,
                &[FirstPlaceKart, TrackArea],
                &[Knockback],
            ),
            ("devil", Status, &[EveryoneExceptSource], &[ReverseSteering]),
            (
                "doctor_r",
                Status,
                &[NonAlliedKarts],
                &[ReverseSteering, ReverseThrottle],
            ),
            (
                "jiangshi",
                Status,
                &[EveryoneExceptSource],
                &[ReverseThrottle],
            ),
            (
                "first_place_devil",
                Status,
                &[FirstPlaceKart],
                &[ReverseSteering],
            ),
            ("slot_lock", Status, &[OpposingTeam], &[LockItemSlots]),
            (
                "scanning",
                Utility,
                &[OwnTeam, OpposingTeam],
                &[RevealItemSlots],
            ),
            ("ghost", Utility, &[SelfKart], &[HideKart]),
            ("oil", Utility, &[TrackArea], &[OilBlind]),
        ];

        for (item, (slug, category, targets, effects)) in
            P5136_GAMEPLAY_ITEM_HINTS.iter().zip(EXPECTED)
        {
            assert_eq!(item.slug, slug);
            assert_eq!(item.category, category, "{slug} category");
            assert_eq!(item.targets, targets, "{slug} targets");
            assert_eq!(item.effects, effects, "{slug} effects");
        }

        let expected_summaries: [(&str, &str); 54] = [
            ("booster", "자신의 카트를 잠시 가속한다."),
            (
                "power_booster",
                "팀 게이지가 차면 일반 부스터에서 변환되는 장시간 부스터다.",
            ),
            ("siren", "사용자를 가속하고 접촉한 카트를 회전시킨다."),
            ("zongzi", "이벤트 모드에서 질주하며 접촉 상대를 밟는다."),
            (
                "magnet",
                "조준한 앞선 카트 쪽으로 사용자를 빠르게 끌어간다.",
            ),
            ("golden_magnet", "자석 가속과 자기 방어를 동시에 적용한다."),
            ("missile", "조준한 카트를 추적해 공중으로 띄운다."),
            (
                "first_place_missile",
                "현재 1위를 자동 추적해 미사일 효과를 준다.",
            ),
            (
                "rocket_launcher",
                "플래그전에서 여러 발의 조준 미사일을 제공한다.",
            ),
            (
                "golden_missile",
                "풍선 방어를 무시하고 조준한 카트를 띄운다.",
            ),
            ("tiger_missile", "조준한 카트의 속도와 시야를 방해한다."),
            (
                "electromagnetic_missile",
                "표적을 전자기장에 가두고 탈출 시 주변 카트를 감속한다.",
            ),
            ("snow_fairy", "표적을 작게 만들고 감속한다."),
            (
                "random_missile",
                "앞선 상대 팀원 한 명을 무작위로 자동 공격한다.",
            ),
            (
                "waterbomb",
                "전방 일정 구역을 폭발시켜 범위 내 카트를 물에 가둔다.",
            ),
            (
                "timed_waterbomb",
                "사용자 위치를 중심으로 지연 폭발하며 사용자도 맞는다.",
            ),
            (
                "infected_waterbomb",
                "범위 내 카트를 물에 가둔 뒤 아이템 슬롯도 잠근다.",
            ),
            ("coke_bomb", "전방 범위의 카트를 콜라 물기둥에 가둔다."),
            ("ice_bomb", "전방 범위의 카트를 얼음에 가둔다."),
            (
                "rolling_waterbomb",
                "트랙을 구르다가 충돌 또는 시간 경과로 물폭탄을 만든다.",
            ),
            (
                "net",
                "전방 구역의 카트를 그물에 가둬 움직이지 못하게 한다.",
            ),
            ("waterfly", "바로 앞 순위 카트를 자동 추적해 물에 가둔다."),
            (
                "ice_waterfly",
                "앞선 카트를 자동 추적해 더 강한 얼음 구속을 건다.",
            ),
            (
                "infected_waterfly",
                "앞선 카트를 물에 가둔 뒤 아이템 슬롯도 잠근다.",
            ),
            (
                "bomb_waterfly",
                "바로 앞 카트에 시한 물폭탄을 붙여 잠시 뒤 주변까지 물에 가둔다.",
            ),
            ("ufo", "현재 1위를 자동 추적해 일정 시간 감속한다."),
            (
                "ufo_carrier",
                "1위를 우주선으로 감속하고 그 주변 카트에도 감속을 건다.",
            ),
            ("thunderbolt", "자신보다 앞선 모든 상대를 동시에 공격한다."),
            (
                "shield",
                "자신에게 들어오는 방어 가능한 공격 한 번을 막는다.",
            ),
            ("angel", "자신과 같은 팀 전체에 방어 효과를 적용한다."),
            (
                "super_shield",
                "자신을 보호하면서 카트 속도도 높이는 강화 실드다.",
            ),
            (
                "golden_shield",
                "지속 시간 동안 방어 가능한 공격을 반복해서 막는다.",
            ),
            (
                "protect_shield",
                "지속 시간 동안 방어 가능한 공격을 반복해서 막는 특수 실드다.",
            ),
            (
                "siren_shield",
                "사이렌 접촉 공격과 자기 방어를 함께 제공한다.",
            ),
            ("emp", "자신 또는 팀에 걸린 우주선 효과를 제거한다."),
            ("cloud", "뒤쪽 트랙에 통과자의 시야를 가리는 구름을 둔다."),
            (
                "ink_cloud",
                "고글로 지속시간을 줄이기 어려운 강화 시야 구름이다.",
            ),
            (
                "new_cloud",
                "한 명이 지나가도 유지되는 무지개 시야 구름이다.",
            ),
            (
                "fairy_cloud",
                "고글을 무시하는 NEW 구름 계열의 테마 변형이다.",
            ),
            ("banana", "밟은 카트를 회전시키는 바닥 트랩을 설치한다."),
            ("big_banana", "더 큰 판정의 바나나 트랩을 설치한다."),
            ("mine", "밟은 카트를 공중으로 띄우는 지뢰를 설치한다."),
            ("duck_bomb", "밟은 카트를 띄우는 오리 모양 지뢰 변형이다."),
            ("water_mine", "밟은 카트를 물에 가두는 지뢰를 설치한다."),
            (
                "booby_trap",
                "밟은 카트를 진행 반대 방향으로 밀어내는 트랩이다.",
            ),
            ("barricade", "1위 전방에 충돌 장애물 여러 개를 설치한다."),
            ("devil", "사용자를 제외한 참가자의 좌우 조향을 반전한다."),
            (
                "doctor_r",
                "리버스 트랙에서 상대의 상하좌우 입력을 반전한다.",
            ),
            (
                "jiangshi",
                "사용자를 제외한 참가자의 전진·후진 입력을 반전한다.",
            ),
            (
                "first_place_devil",
                "현재 1위 한 명의 좌우 조향을 반전한다.",
            ),
            (
                "slot_lock",
                "상대 팀 전원의 아이템 슬롯 사용을 잠시 막는다.",
            ),
            (
                "scanning",
                "자신의 팀이 상대 팀의 현재 아이템 슬롯을 보게 한다.",
            ),
            (
                "ghost",
                "사용자를 상대 시야에서 숨기되 충돌과 공격 판정은 유지한다.",
            ),
            ("oil", "밟은 카트의 시야를 가리는 바닥 기름을 설치한다."),
        ];
        assert_eq!(
            P5136_GAMEPLAY_ITEM_HINTS
                .iter()
                .map(|item| (item.slug, item.effect_summary_ko))
                .collect::<Vec<_>>(),
            expected_summaries
        );

        assert_eq!(
            gameplay_item_by_slug("power_booster").and_then(|item| item.documented_availability()),
            Some(&[GameplayAvailabilityHint::SpeedTeam][..])
        );
        assert_eq!(
            gameplay_item_by_slug("rocket_launcher")
                .and_then(|item| item.documented_availability()),
            Some(
                &[
                    GameplayAvailabilityHint::FlagIndividual,
                    GameplayAvailabilityHint::FlagTeam,
                ][..]
            )
        );
        assert_eq!(
            gameplay_item_by_slug("doctor_r").and_then(|item| item.documented_availability()),
            Some(&[GameplayAvailabilityHint::ReverseTrackOnly][..])
        );
        assert_eq!(
            gameplay_item_by_slug("random_missile").and_then(|item| item.documented_availability()),
            Some(&[GameplayAvailabilityHint::ItemTeam][..])
        );
        assert_eq!(
            gameplay_item_by_slug("slot_lock").and_then(|item| item.documented_availability()),
            Some(
                &[
                    GameplayAvailabilityHint::ItemTeam,
                    GameplayAvailabilityHint::ItemTeamBattle,
                ][..]
            )
        );
        assert_eq!(
            gameplay_item_by_slug("scanning").and_then(|item| item.documented_availability()),
            Some(
                &[
                    GameplayAvailabilityHint::ItemTeam,
                    GameplayAvailabilityHint::ItemTeamBattle,
                ][..]
            )
        );
        assert_eq!(
            gameplay_item_by_slug("booster").and_then(|item| item.documented_availability()),
            None
        );
        assert_eq!(
            P5136_GAMEPLAY_ITEM_HINTS
                .iter()
                .filter(|item| item.documented_availability().is_some())
                .count(),
            6
        );
    }

    // Page-to-class associations are a separate literal oracle from both the
    // native class census and the item-ID manifest.
    #[allow(clippy::too_many_lines)]
    #[test]
    fn operation_associations_and_coverage_are_exact() {
        use ItemOperationClassEvidence::{CSharpRelayOnly, NativeWriterSchema};
        use OperationLinkEvidence::{AmbiguousCandidate, NameCorrelation, VerifiedAssociation};
        use P5136CoverageLevel::{
            GameplayReferenceOnly, OperationCandidate, VerifiedItemSymbol, VerifiedOperation,
        };

        type LinkGolden = (
            &'static str,
            &'static str,
            OperationLinkEvidence,
            ItemOperationClassEvidence,
        );
        const EXPECTED_LINKS: [LinkGolden; 53] = [
            ("siren", "GopSiren", VerifiedAssociation, NativeWriterSchema),
            (
                "magnet",
                "GopMagnet",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "golden_magnet",
                "GopSuperMag",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "missile",
                "GopRocket",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "first_place_missile",
                "GopRocket",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "rocket_launcher",
                "GopRocket",
                NameCorrelation,
                NativeWriterSchema,
            ),
            (
                "golden_missile",
                "GopGoldRocket",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "tiger_missile",
                "GopTigerRocket",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "electromagnetic_missile",
                "GopLockdownRocket",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "snow_fairy",
                "GopSnowman",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "waterbomb",
                "GopWaterbomb",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "timed_waterbomb",
                "GopTimebomb",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "infected_waterbomb",
                "GopInfectedBomb",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "coke_bomb",
                "GopCokebomb",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "ice_bomb",
                "GopSnowbomb",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "rolling_waterbomb",
                "GopRollingbomb",
                AmbiguousCandidate,
                NativeWriterSchema,
            ),
            (
                "rolling_waterbomb",
                "GopRollingCokebomb",
                AmbiguousCandidate,
                NativeWriterSchema,
            ),
            (
                "waterfly",
                "GopWaterfly",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "ice_waterfly",
                "GopSnowWaterfly",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "infected_waterfly",
                "GopInfectedWaterfly",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "bomb_waterfly",
                "GopWaterbombFly",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            ("ufo", "GopUfo", VerifiedAssociation, NativeWriterSchema),
            (
                "ufo_carrier",
                "GopAreaUfo",
                NameCorrelation,
                NativeWriterSchema,
            ),
            (
                "thunderbolt",
                "GopThunderbolt",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "shield",
                "GopShield",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            ("angel", "GopAngel", VerifiedAssociation, NativeWriterSchema),
            (
                "super_shield",
                "GopShield",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "golden_shield",
                "GopGoldShield",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "protect_shield",
                "GopGoldShield",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "siren_shield",
                "GopSirenShield",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "siren_shield",
                "GopGoldShield",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            ("emp", "GopEmp", VerifiedAssociation, NativeWriterSchema),
            ("cloud", "GopCloud", VerifiedAssociation, NativeWriterSchema),
            (
                "ink_cloud",
                "GopCloud2",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "new_cloud",
                "GopCloud2",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "fairy_cloud",
                "GopCloud",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "banana",
                "GopBanana",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "big_banana",
                "GopBanana",
                NameCorrelation,
                NativeWriterSchema,
            ),
            ("mine", "GopMine", VerifiedAssociation, NativeWriterSchema),
            ("duck_bomb", "GopMine", NameCorrelation, NativeWriterSchema),
            (
                "water_mine",
                "GopWaterMine",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "booby_trap",
                "GopForceZone",
                NameCorrelation,
                NativeWriterSchema,
            ),
            (
                "barricade",
                "GopBarricade",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            ("devil", "GopDevil", VerifiedAssociation, NativeWriterSchema),
            ("doctor_r", "GopDrmad", NameCorrelation, CSharpRelayOnly),
            (
                "jiangshi",
                "GopMqDevil",
                AmbiguousCandidate,
                NativeWriterSchema,
            ),
            (
                "jiangshi",
                "GopNewDevil",
                AmbiguousCandidate,
                NativeWriterSchema,
            ),
            (
                "first_place_devil",
                "GopMqDevil",
                AmbiguousCandidate,
                NativeWriterSchema,
            ),
            (
                "first_place_devil",
                "GopNewDevil",
                AmbiguousCandidate,
                NativeWriterSchema,
            ),
            (
                "slot_lock",
                "GopSlotLock",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            (
                "scanning",
                "GopScanning",
                VerifiedAssociation,
                NativeWriterSchema,
            ),
            ("ghost", "GopGhost", VerifiedAssociation, NativeWriterSchema),
            ("oil", "GopOil", VerifiedAssociation, NativeWriterSchema),
        ];
        let actual_links = P5136_GAMEPLAY_ITEM_HINTS
            .iter()
            .flat_map(|item| {
                item.operation_links.iter().map(|link| {
                    (
                        item.slug,
                        link.class_name,
                        link.evidence,
                        link.class_evidence().expect("catalog class must be known"),
                    )
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(actual_links, EXPECTED_LINKS);

        let expected_coverage: [(&str, P5136CoverageLevel); 54] = [
            ("booster", VerifiedItemSymbol),
            ("power_booster", GameplayReferenceOnly),
            ("siren", VerifiedOperation),
            ("zongzi", GameplayReferenceOnly),
            ("magnet", VerifiedOperation),
            ("golden_magnet", VerifiedOperation),
            ("missile", VerifiedOperation),
            ("first_place_missile", VerifiedOperation),
            ("rocket_launcher", OperationCandidate),
            ("golden_missile", VerifiedOperation),
            ("tiger_missile", VerifiedOperation),
            ("electromagnetic_missile", VerifiedOperation),
            ("snow_fairy", VerifiedOperation),
            ("random_missile", GameplayReferenceOnly),
            ("waterbomb", VerifiedOperation),
            ("timed_waterbomb", VerifiedOperation),
            ("infected_waterbomb", VerifiedOperation),
            ("coke_bomb", VerifiedOperation),
            ("ice_bomb", VerifiedOperation),
            ("rolling_waterbomb", VerifiedItemSymbol),
            ("net", GameplayReferenceOnly),
            ("waterfly", VerifiedOperation),
            ("ice_waterfly", VerifiedOperation),
            ("infected_waterfly", VerifiedOperation),
            ("bomb_waterfly", VerifiedOperation),
            ("ufo", VerifiedOperation),
            ("ufo_carrier", OperationCandidate),
            ("thunderbolt", VerifiedOperation),
            ("shield", VerifiedOperation),
            ("angel", VerifiedOperation),
            ("super_shield", VerifiedOperation),
            ("golden_shield", VerifiedOperation),
            ("protect_shield", VerifiedOperation),
            ("siren_shield", VerifiedOperation),
            ("emp", VerifiedOperation),
            ("cloud", VerifiedOperation),
            ("ink_cloud", VerifiedOperation),
            ("new_cloud", VerifiedOperation),
            ("fairy_cloud", VerifiedOperation),
            ("banana", VerifiedOperation),
            ("big_banana", VerifiedItemSymbol),
            ("mine", VerifiedOperation),
            ("duck_bomb", VerifiedItemSymbol),
            ("water_mine", VerifiedOperation),
            ("booby_trap", OperationCandidate),
            ("barricade", VerifiedOperation),
            ("devil", VerifiedOperation),
            ("doctor_r", OperationCandidate),
            ("jiangshi", OperationCandidate),
            ("first_place_devil", OperationCandidate),
            ("slot_lock", VerifiedOperation),
            ("scanning", VerifiedOperation),
            ("ghost", VerifiedOperation),
            ("oil", VerifiedOperation),
        ];
        assert_eq!(
            P5136_GAMEPLAY_ITEM_HINTS
                .iter()
                .map(|item| (item.slug, item.p5136_coverage()))
                .collect::<Vec<_>>(),
            expected_coverage
        );
    }

    // The complete ID/name/evidence manifest is more useful than a generated
    // self-consistency check, even though the fixture is necessarily long.
    #[allow(clippy::too_many_lines)]
    #[test]
    fn p5136_numeric_links_are_unique_and_searchable() {
        const EXPECTED_LINKS: [(i16, &str, ItemSymbolEvidence); 41] = [
            (6, "booster", FALLBACK),
            (24, "siren", SUPPLEMENT),
            (5, "magnet", FALLBACK),
            (103, "superMagnet", SUPPLEMENT),
            (7, "rocket", FALLBACK),
            (33, "guideRocket", FALLBACK),
            (32, "goldRocket", EXECUTABLE),
            (99, "tigerRocket", EXECUTABLE),
            (104, "lockdownRocket", EXECUTABLE),
            (112, "snowman", EXECUTABLE),
            (9, "waterBomb", FALLBACK),
            (13, "timeBomb", FALLBACK),
            (27, "infectedBomb", EXECUTABLE),
            (20, "cokeBomb", EXECUTABLE),
            (34, "snowBomb", EXECUTABLE),
            (22, "rollingCokeBomb", EXECUTABLE),
            (4, "waterFly", FALLBACK),
            (118, "snowWaterFly", EXECUTABLE),
            (119, "infectedWaterFly", EXECUTABLE),
            (120, "waterbombFly", EXECUTABLE),
            (3, "ufo", FALLBACK),
            (111, "thunderbolt", FALLBACK),
            (10, "shield", FALLBACK),
            (11, "angel", FALLBACK),
            (18, "superShield", FALLBACK),
            (36, "goldShield", EXECUTABLE),
            (81, "protectShield", EXECUTABLE),
            (106, "sirenShield", EXECUTABLE),
            (12, "emp", FALLBACK),
            (1, "darkCloud", EXECUTABLE),
            (115, "darkCloud2", EXECUTABLE),
            (114, "cloud2", FALLBACK),
            (43, "rainbowCloud", EXECUTABLE),
            (8, "banana", FALLBACK),
            (85, "bigBanana", EXECUTABLE),
            (45, "duckMine", EXECUTABLE),
            (37, "waterMine", EXECUTABLE),
            (113, "barricade", FALLBACK),
            (2, "devil", FALLBACK),
            (110, "slotLock", FALLBACK),
            (109, "scanning", FALLBACK),
        ];

        let actual_links = P5136_GAMEPLAY_ITEM_HINTS
            .iter()
            .flat_map(|item| item.item_symbols)
            .map(|link| (link.item_id, link.symbol, link.evidence))
            .collect::<Vec<_>>();
        assert_eq!(actual_links, EXPECTED_LINKS);
        assert_eq!(
            actual_links
                .iter()
                .filter(|(_, _, evidence)| *evidence == FALLBACK)
                .count(),
            19
        );
        assert_eq!(
            actual_links
                .iter()
                .filter(|(_, _, evidence)| *evidence == EXECUTABLE)
                .count(),
            20
        );
        assert_eq!(
            actual_links
                .iter()
                .filter(|(_, _, evidence)| *evidence == SUPPLEMENT)
                .count(),
            2
        );

        let mut ids = HashSet::new();
        let mut symbols = HashSet::new();
        for item in P5136_GAMEPLAY_ITEM_HINTS {
            assert!(!item.targets.is_empty());
            assert!(!item.effects.is_empty());
            for link in item.item_symbols {
                assert!(
                    ids.insert(link.item_id),
                    "duplicate item id {}",
                    link.item_id
                );
                assert!(
                    symbols.insert(link.symbol.to_ascii_lowercase()),
                    "duplicate item symbol {}",
                    link.symbol
                );
                assert_eq!(gameplay_item_by_id(link.item_id), Some(item));
                assert_eq!(gameplay_item_by_symbol(link.symbol), Some(item));
            }
            for link in item.operation_links {
                assert!(
                    link.class_evidence().is_some(),
                    "{} links unknown operation class {}",
                    item.slug,
                    link.class_name
                );
            }
        }
        assert_eq!(
            gameplay_item_by_id(6).map(|item| item.slug),
            Some("booster")
        );
        assert_eq!(gameplay_item_by_id(11).map(|item| item.slug), Some("angel"));
        assert_eq!(
            gameplay_item_by_id(109).map(|item| item.slug),
            Some("scanning")
        );
        assert_eq!(
            gameplay_item_by_slug("slot_lock").map(|item| item.korean_name),
            Some("자물쇠")
        );
        assert_eq!(
            gameplay_item_by_slug("zongzi").map(|item| item.p5136_coverage()),
            Some(P5136CoverageLevel::GameplayReferenceOnly)
        );
        assert_eq!(
            gameplay_item_by_slug("golden_shield").map(|item| item.p5136_coverage()),
            Some(P5136CoverageLevel::VerifiedOperation)
        );
        assert_eq!(
            gameplay_item_by_slug("ufo_carrier").map(|item| item.p5136_coverage()),
            Some(P5136CoverageLevel::OperationCandidate)
        );
        assert_eq!(
            gameplay_item_by_slug("rolling_waterbomb").map(|item| item.p5136_coverage()),
            Some(P5136CoverageLevel::VerifiedItemSymbol)
        );
        assert_eq!(
            gameplay_item_by_slug("slot_lock").map(|item| item.p5136_coverage()),
            Some(P5136CoverageLevel::VerifiedOperation)
        );
    }
}
