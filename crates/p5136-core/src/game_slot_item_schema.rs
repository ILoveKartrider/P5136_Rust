//! Static Korean P5136 `GameSlotPacket` type-12 writer schemas.
//!
//! The native writer census is useful wire evidence, but it is not equivalent
//! to a complete live compatibility trace. Each successful validation therefore
//! carries an [`ItemOperationEvidence`] value for diagnostics. Relay admission
//! belongs to the outer `GameSlot` envelope: a known operation pair may be
//! relayed only after its bounded envelope, authenticated sender, and audience
//! mask have all been validated.

use thiserror::Error;

/// Maximum nested raw body accepted by the outer `GameSlot` codec.
pub const MAX_ITEM_OPERATION_RAW_LENGTH: usize = 0x3c0;
pub const P5136_TYPE12_SCHEMA_COUNT: usize = 67;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemOperationEvidence {
    /// The exact class/state/shape occurs in the retained encrypted TCP traces.
    RetainedTrace,
    /// A native writer has an explicit branch for this state and length.
    StaticWriterBranch,
    /// The writer's default or state-independent path has this length.
    ///
    /// This proves a serializer shape, not that an arbitrary state is reachable
    /// or safe for a peer's class-specific native handler.
    StaticWriterDefault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemOperationStateField {
    U8 { offset: usize },
    U32 { offset: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateLengthCase {
    pub states: &'static [u32],
    pub length: usize,
    pub evidence: ItemOperationEvidence,
}

impl StateLengthCase {
    #[must_use]
    pub const fn new(
        states: &'static [u32],
        length: usize,
        evidence: ItemOperationEvidence,
    ) -> Self {
        Self {
            states,
            length,
            evidence,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemOperationLengthRule {
    /// The native writer emits one length independently of its state field.
    Fixed {
        length: usize,
        evidence: ItemOperationEvidence,
    },
    /// Explicit writer branches plus a serializer-default length.
    StateMap {
        cases: &'static [StateLengthCase],
        default_length: usize,
    },
    /// `GopCourse`: `24 + 2 * count`, with the count at raw offset 16.
    CountedAnyState {
        count_offset: usize,
        base_length: usize,
        element_width: usize,
    },
    /// One state carries a bounded count while other explicit states are fixed.
    CountedState {
        counted_state: u32,
        count_offset: usize,
        base_length: usize,
        element_width: usize,
        cases: &'static [StateLengthCase],
        default_length: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemOperationSchema {
    pub class_name: &'static str,
    pub operation_hash: u32,
    pub base_hash: u32,
    pub state_field: ItemOperationStateField,
    pub length_rule: ItemOperationLengthRule,
}

impl ItemOperationSchema {
    #[must_use]
    pub const fn mapped_u32(
        class_name: &'static str,
        operation_hash: u32,
        base_hash: u32,
        state_offset: usize,
        cases: &'static [StateLengthCase],
        default_length: usize,
    ) -> Self {
        Self {
            class_name,
            operation_hash,
            base_hash,
            state_field: ItemOperationStateField::U32 {
                offset: state_offset,
            },
            length_rule: ItemOperationLengthRule::StateMap {
                cases,
                default_length,
            },
        }
    }

    #[must_use]
    pub const fn mapped_u8(
        class_name: &'static str,
        operation_hash: u32,
        base_hash: u32,
        state_offset: usize,
        cases: &'static [StateLengthCase],
        default_length: usize,
    ) -> Self {
        Self {
            class_name,
            operation_hash,
            base_hash,
            state_field: ItemOperationStateField::U8 {
                offset: state_offset,
            },
            length_rule: ItemOperationLengthRule::StateMap {
                cases,
                default_length,
            },
        }
    }

    #[must_use]
    pub const fn fixed_u32(
        class_name: &'static str,
        operation_hash: u32,
        base_hash: u32,
        state_offset: usize,
        length: usize,
    ) -> Self {
        Self {
            class_name,
            operation_hash,
            base_hash,
            state_field: ItemOperationStateField::U32 {
                offset: state_offset,
            },
            length_rule: ItemOperationLengthRule::Fixed {
                length,
                evidence: ItemOperationEvidence::StaticWriterDefault,
            },
        }
    }

    #[must_use]
    pub const fn fixed_u8(
        class_name: &'static str,
        operation_hash: u32,
        base_hash: u32,
        state_offset: usize,
        length: usize,
    ) -> Self {
        Self {
            class_name,
            operation_hash,
            base_hash,
            state_field: ItemOperationStateField::U8 {
                offset: state_offset,
            },
            length_rule: ItemOperationLengthRule::Fixed {
                length,
                evidence: ItemOperationEvidence::StaticWriterDefault,
            },
        }
    }

    #[must_use]
    pub const fn pair(self) -> (u32, u32) {
        (self.operation_hash, self.base_hash)
    }

    pub fn validate(
        &'static self,
        raw: &[u8],
    ) -> Result<ValidatedItemOperation, ItemOperationValidationError> {
        let object_id = read_u32(raw, 8, self.class_name, "object ID")?;
        if object_id == u32::MAX {
            return Err(ItemOperationValidationError::MissingObjectId {
                class_name: self.class_name,
            });
        }
        let state = match self.state_field {
            ItemOperationStateField::U8 { offset } => u32::from(*raw.get(offset).ok_or(
                ItemOperationValidationError::TruncatedField {
                    class_name: self.class_name,
                    field: "state byte",
                    offset,
                    width: 1,
                    actual: raw.len(),
                },
            )?),
            ItemOperationStateField::U32 { offset } => {
                read_u32(raw, offset, self.class_name, "state dword")?
            }
        };

        let (expected, evidence) = self.expected_length(raw, state)?;
        if raw.len() != expected {
            return Err(ItemOperationValidationError::InvalidLength {
                class_name: self.class_name,
                state,
                actual: raw.len(),
                expected,
            });
        }
        Ok(ValidatedItemOperation {
            schema: self,
            object_id,
            state,
            evidence,
        })
    }

    fn expected_length(
        self,
        raw: &[u8],
        state: u32,
    ) -> Result<(usize, ItemOperationEvidence), ItemOperationValidationError> {
        match self.length_rule {
            ItemOperationLengthRule::Fixed { length, evidence } => Ok((length, evidence)),
            ItemOperationLengthRule::StateMap {
                cases,
                default_length,
            } => Ok(cases
                .iter()
                .find(|case| case.states.contains(&state))
                .map_or(
                    (default_length, ItemOperationEvidence::StaticWriterDefault),
                    |case| (case.length, case.evidence),
                )),
            ItemOperationLengthRule::CountedAnyState {
                count_offset,
                base_length,
                element_width,
            } => {
                let count = read_u32(raw, count_offset, self.class_name, "element count")?;
                let expected = counted_length(self.class_name, count, base_length, element_width)?;
                let evidence =
                    if self.class_name == "GopCourse" && matches!(state, 0 | 1) && count == 4 {
                        ItemOperationEvidence::RetainedTrace
                    } else {
                        ItemOperationEvidence::StaticWriterDefault
                    };
                Ok((expected, evidence))
            }
            ItemOperationLengthRule::CountedState {
                counted_state,
                count_offset,
                base_length,
                element_width,
                cases,
                default_length,
            } => {
                if state == counted_state {
                    let count = read_u32(raw, count_offset, self.class_name, "element count")?;
                    return Ok((
                        counted_length(self.class_name, count, base_length, element_width)?,
                        ItemOperationEvidence::StaticWriterBranch,
                    ));
                }
                Ok(cases
                    .iter()
                    .find(|case| case.states.contains(&state))
                    .map_or(
                        (default_length, ItemOperationEvidence::StaticWriterDefault),
                        |case| (case.length, case.evidence),
                    ))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedItemOperation {
    pub schema: &'static ItemOperationSchema,
    pub object_id: u32,
    pub state: u32,
    pub evidence: ItemOperationEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ItemOperationValidationError {
    #[error(
        "{class_name} {field} at raw offset {offset} needs {width} bytes, but raw length is {actual}"
    )]
    TruncatedField {
        class_name: &'static str,
        field: &'static str,
        offset: usize,
        width: usize,
        actual: usize,
    },

    #[error("{class_name} carries the native missing object ID -1")]
    MissingObjectId { class_name: &'static str },

    #[error(
        "{class_name} state {state} has raw length {actual}; the static writer shape requires {expected}"
    )]
    InvalidLength {
        class_name: &'static str,
        state: u32,
        actual: usize,
        expected: usize,
    },

    #[error(
        "{class_name} count {count} requires raw length {required}, above the P5136 cap {maximum}"
    )]
    CountOverCap {
        class_name: &'static str,
        count: u32,
        required: usize,
        maximum: usize,
    },

    #[error("{class_name} count {count} overflows raw-length arithmetic")]
    CountLengthOverflow {
        class_name: &'static str,
        count: u32,
    },
}

fn counted_length(
    class_name: &'static str,
    count: u32,
    base_length: usize,
    element_width: usize,
) -> Result<usize, ItemOperationValidationError> {
    let count_usize = usize::try_from(count)
        .map_err(|_| ItemOperationValidationError::CountLengthOverflow { class_name, count })?;
    let required = count_usize
        .checked_mul(element_width)
        .and_then(|elements| base_length.checked_add(elements))
        .ok_or(ItemOperationValidationError::CountLengthOverflow { class_name, count })?;
    if required > MAX_ITEM_OPERATION_RAW_LENGTH {
        return Err(ItemOperationValidationError::CountOverCap {
            class_name,
            count,
            required,
            maximum: MAX_ITEM_OPERATION_RAW_LENGTH,
        });
    }
    Ok(required)
}

fn read_u32(
    raw: &[u8],
    offset: usize,
    class_name: &'static str,
    field: &'static str,
) -> Result<u32, ItemOperationValidationError> {
    let bytes =
        raw.get(offset..offset + 4)
            .ok_or(ItemOperationValidationError::TruncatedField {
                class_name,
                field,
                offset,
                width: 4,
                actual: raw.len(),
            })?;
    let array =
        <[u8; 4]>::try_from(bytes).map_err(|_| ItemOperationValidationError::TruncatedField {
            class_name,
            field,
            offset,
            width: 4,
            actual: raw.len(),
        })?;
    Ok(u32::from_le_bytes(array))
}

const TRACE: ItemOperationEvidence = ItemOperationEvidence::RetainedTrace;
const STATIC: ItemOperationEvidence = ItemOperationEvidence::StaticWriterBranch;

macro_rules! c {
    (trace $states:expr, $length:expr) => {
        StateLengthCase::new($states, $length, TRACE)
    };
    ($states:expr, $length:expr) => {
        StateLengthCase::new($states, $length, STATIC)
    };
}

/// Candidate schemas recovered from the native writer census.
///
/// A successful schema validation enriches an operation with its object/state
/// diagnostics. A known-pair operation that does not fit a recovered writer
/// shape can still use the bounded compatibility relay path in the outer codec.
pub static P5136_TYPE12_SCHEMAS: &[ItemOperationSchema] = &[
    ItemOperationSchema::mapped_u32(
        "GopAngel",
        0x0D49_030D,
        0x184D_042C,
        12,
        &[c!(&[0], 25), c!(&[2], 28)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopAreaUfo",
        0x1457_03C9,
        0x2199_04E8,
        12,
        &[c!(&[1], 33), c!(&[2], 24)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopBalloon",
        0x14A6_03ED,
        0x21E8_050C,
        12,
        &[c!(&[1], 28)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopBanana",
        0x1090_0367,
        0x1CB3_0486,
        12,
        &[c!(&[1], 74), c!(trace & [2], 30), c!(&[3], 30)],
        16,
    ),
    ItemOperationSchema::mapped_u8(
        "GopBarricade",
        0x1D86_04A3,
        0x2D06_05C2,
        12,
        &[c!(trace & [1], 73), c!(trace & [2, 3], 25), c!(&[4], 26)],
        25,
    ),
    ItemOperationSchema::fixed_u8("GopBigTimebomb", 0x276B_0567, 0x3929_0686, 12, 29),
    ItemOperationSchema::mapped_u32(
        "GopBlock",
        0x0D59_0311,
        0x185D_0430,
        16,
        &[c!(&[1], 89), c!(&[2], 29)],
        20,
    ),
    ItemOperationSchema::mapped_u32(
        "GopBossPrison",
        0x233A_0538,
        0x33D9_0657,
        12,
        &[c!(&[1], 77), c!(&[2], 64), c!(&[3], 68)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopBoundRoad",
        0x1DB9_04A4,
        0x2D39_05C3,
        16,
        &[c!(&[1], 63), c!(&[2, 3], 33)],
        20,
    ),
    ItemOperationSchema::mapped_u32(
        "GopBoundWall",
        0x1DC1_04AE,
        0x2D41_05CD,
        16,
        &[c!(&[1], 137), c!(&[2, 3], 29)],
        20,
    ),
    ItemOperationSchema::mapped_u32(
        "GopCloud",
        0x0D7B_031D,
        0x187F_043C,
        12,
        &[c!(&[1], 73), c!(&[2], 20)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopCokeRocket",
        0x2261_0510,
        0x3300_062F,
        12,
        &[
            c!(&[1], 77),
            c!(&[2], 72),
            c!(&[3, 4], 20),
            c!(&[5, 6], 16),
            c!(&[7], 24),
            c!(&[8, 9, 10], 20),
        ],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopCokebomb",
        0x1900_0448,
        0x2761_0567,
        12,
        &[c!(&[1], 120), c!(&[2, 3, 4], 28)],
        16,
    ),
    ItemOperationSchema {
        class_name: "GopCourse",
        operation_hash: 0x1139_0397,
        base_hash: 0x0D73_0327,
        state_field: ItemOperationStateField::U32 { offset: 12 },
        length_rule: ItemOperationLengthRule::CountedAnyState {
            count_offset: 16,
            base_length: 24,
            element_width: 2,
        },
    },
    ItemOperationSchema::mapped_u32(
        "GopCube",
        0x0A4F_02A5,
        0x1434_03C4,
        12,
        &[c!(&[1], 24), c!(&[2], 28)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopCubeForBoss",
        0x273E_0563,
        0x38FC_0682,
        12,
        &[c!(&[1], 73)],
        65,
    ),
    ItemOperationSchema::mapped_u32(
        "GopDinoClawRocket",
        0x3A06_069F,
        0x4F21_07BE,
        12,
        &[
            c!(&[1], 77),
            c!(&[2], 72),
            c!(&[3, 4], 20),
            c!(&[5, 6], 16),
            c!(&[7], 24),
            c!(&[8, 9, 10], 20),
        ],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopDynamite",
        0x1977_0461,
        0x27D8_0580,
        12,
        &[c!(&[1], 30), c!(&[2], 29)],
        16,
    ),
    ItemOperationSchema::fixed_u32("GopEventObject", 0x2856_057F, 0x3A14_069E, 12, 20),
    ItemOperationSchema::mapped_u32(
        "GopFalling",
        0x14A7_03E3,
        0x21E9_0502,
        16,
        &[c!(&[1], 91), c!(&[2, 3], 33)],
        20,
    ),
    ItemOperationSchema::mapped_u32(
        "GopForceZone",
        0x1DC6_04B1,
        0x2D46_05D0,
        12,
        &[c!(&[1], 72), c!(&[2, 3], 29), c!(&[5], 25)],
        16,
    ),
    ItemOperationSchema::fixed_u32("GopGiantTalisman", 0x3442_0652, 0x483E_0771, 12, 28),
    ItemOperationSchema::mapped_u32(
        "GopGoldRocket",
        0x228A_0514,
        0x3329_0633,
        12,
        &[
            c!(&[1], 77),
            c!(&[2], 72),
            c!(&[3], 22),
            c!(&[4], 20),
            c!(&[5, 6], 16),
            c!(&[7], 24),
            c!(&[8, 9, 10], 20),
        ],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopHammer",
        0x10D3_0380,
        0x1CF6_049F,
        12,
        &[c!(&[1], 30), c!(&[2], 29)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopHeadBand",
        0x17FB_040D,
        0x265C_052C,
        12,
        &[c!(&[1], 21)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopIcefly",
        0x10C3_0382,
        0x1CE6_04A1,
        12,
        &[c!(&[1], 78)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopInfectedBomb",
        0x2DC1_05C8,
        0x409E_06E7,
        12,
        &[c!(&[1], 121), c!(&[2, 3], 33)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopInfectedWaterfly",
        0x49AB_0796,
        0x6104_08B5,
        12,
        &[
            c!(&[1], 77),
            c!(&[2], 65),
            c!(&[3], 68),
            c!(&[4], 28),
            c!(&[5], 20),
        ],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopItemTimeFlybomb",
        0x41CC_070F,
        0x3996_067F,
        12,
        &[c!(&[1], 24), c!(&[2], 28), c!(&[3, 4], 24)],
        16,
    ),
    ItemOperationSchema::mapped_u8(
        "GopLockdownRocket",
        0x3BEA_06CF,
        0x5105_07EE,
        12,
        &[
            c!(&[1], 20),
            c!(&[2], 17),
            c!(&[4, 9], 25),
            c!(&[5, 6], 18),
            c!(&[7, 8], 26),
        ],
        13,
    ),
    ItemOperationSchema::mapped_u32(
        "GopLucci",
        0x0D89_0316,
        0x0A33_02A6,
        12,
        &[c!(&[0], 74), c!(&[1], 25)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopMine",
        0x0A6B_02AF,
        0x1450_03CE,
        12,
        &[
            c!(&[1], 77),
            c!(trace & [2], 29),
            c!(&[3, 4, 5], 29),
            c!(&[6], 68),
        ],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopMovingUfo",
        0x1E52_04C0,
        0x2DD2_05DF,
        12,
        &[c!(&[1], 72), c!(&[2], 24)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopOil",
        0x07C0_024A,
        0x1086_0369,
        12,
        &[c!(&[1], 73), c!(&[2], 29), c!(&[3], 25)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopPiratebomb",
        0x2369_052B,
        0x3408_064A,
        12,
        &[c!(&[1, 2, 3, 4], 28)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopPress",
        0x0DC1_0333,
        0x18C5_0452,
        12,
        &[c!(&[1], 72), c!(&[2], 28)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopRobotBeam",
        0x1DC5_04A1,
        0x2D45_05C0,
        12,
        &[c!(&[1], 72), c!(&[2], 24)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopRocket",
        0x1129_038E,
        0x1D4C_04AD,
        12,
        &[
            c!(&[1], 82),
            c!(trace & [2], 73),
            c!(&[3, 4], 20),
            c!(&[5, 6], 16),
            c!(&[7], 24),
            c!(&[8, 9, 10], 20),
        ],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopRollingCokebomb",
        0x42E4_071F,
        0x591E_083E,
        12,
        &[c!(&[1], 132), c!(&[2], 28), c!(&[3, 4], 24)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopRollingInfectedbomb",
        0x6381_08BF,
        0x7E37_09DE,
        12,
        &[c!(&[1], 132), c!(&[2], 32), c!(&[3], 28)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopRollingbomb",
        0x2954_059D,
        0x3B12_06BC,
        12,
        &[c!(&[1], 132), c!(&[2], 28), c!(&[3, 4], 24)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopShield",
        0x1110_037F,
        0x1D33_049E,
        12,
        &[c!(&[1], 31), c!(&[2], 29)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopSilence",
        0x150D_03E9,
        0x224F_0508,
        12,
        &[c!(&[1, 2], 29)],
        17,
    ),
    ItemOperationSchema::mapped_u32(
        "GopSiren",
        0x0DB2_0327,
        0x18B6_0446,
        12,
        &[c!(&[1], 26), c!(&[2], 31)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopSirenShield",
        0x28A5_0580,
        0x3A63_069F,
        12,
        &[c!(&[0, 2], 25), c!(&[1], 24)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopSnowWaterfly",
        0x2F69_061B,
        0x4246_073A,
        12,
        &[
            c!(&[1], 77),
            c!(&[2], 65),
            c!(&[3], 68),
            c!(&[4], 28),
            c!(&[5], 20),
        ],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopSnowbomb",
        0x19EB_046D,
        0x284C_058C,
        12,
        &[c!(&[1], 120), c!(&[2, 3, 4], 28)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopSnowman",
        0x1584_0409,
        0x22C6_0528,
        12,
        &[
            c!(&[1], 77),
            c!(&[2], 72),
            c!(&[3, 4], 20),
            c!(&[5, 6], 16),
            c!(&[7], 24),
            c!(&[8, 9, 10], 20),
        ],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopSpaceCraft",
        0x2262_0502,
        0x3301_0621,
        12,
        &[c!(&[0], 30), c!(&[2, 3, 4, 5], 29), c!(&[7], 17)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopSpecialShield",
        0x3473_0640,
        0x486F_075F,
        12,
        &[c!(&[0], 27), c!(&[2, 3], 25)],
        17,
    ),
    ItemOperationSchema::mapped_u32(
        "GopSpecialSmall",
        0x2E3D_05E0,
        0x411A_06FF,
        12,
        &[c!(&[0], 30), c!(&[1], 29), c!(&[2], 17)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopSuperMag",
        0x198F_044A,
        0x27F0_0569,
        12,
        &[c!(&[1], 29)],
        16,
    ),
    ItemOperationSchema::fixed_u32("GopTargetKart", 0x22FA_051F, 0x1D74_04AF, 12, 20),
    ItemOperationSchema {
        class_name: "GopThunderbolt",
        operation_hash: 0x2973_05B1,
        base_hash: 0x3B31_06D0,
        state_field: ItemOperationStateField::U32 { offset: 12 },
        length_rule: ItemOperationLengthRule::CountedState {
            counted_state: 1,
            count_offset: 25,
            base_length: 30,
            element_width: 4,
            cases: &[c!(&[2], 25), c!(&[3], 29)],
            default_length: 16,
        },
    },
    ItemOperationSchema::mapped_u32(
        "GopTigerRocket",
        0x2882_0589,
        0x3A40_06A8,
        12,
        &[
            c!(&[1], 77),
            c!(&[2], 72),
            c!(&[3, 4], 20),
            c!(&[5, 6], 16),
            c!(&[7], 24),
            c!(&[8, 9, 10], 20),
        ],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopTimeCokebomb",
        0x2DDA_05D7,
        0x40B7_06F6,
        12,
        &[c!(&[1], 24), c!(&[2], 28), c!(&[3, 4], 24)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopTimeInfectedBomb",
        0x48D7_0757,
        0x6030_0876,
        12,
        &[c!(&[1], 24), c!(&[2], 32), c!(&[3], 28)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopTimeMine",
        0x1909_043E,
        0x276A_055D,
        16,
        &[c!(&[1], 85), c!(&[2, 3, 4], 33), c!(&[5], 24)],
        20,
    ),
    ItemOperationSchema::mapped_u32(
        "GopTimeSnowbomb",
        0x2EC5_05FC,
        0x41A2_071B,
        12,
        &[c!(&[1], 24), c!(&[2, 3, 4], 28)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopTimebomb",
        0x196A_0455,
        0x27CB_0574,
        12,
        &[c!(&[1], 24), c!(&[2, 3, 4], 28)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopTombStone",
        0x1E29_04C1,
        0x2DA9_05E0,
        12,
        &[c!(&[1], 72), c!(&[2], 24)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopUfo",
        0x07CF_0250,
        0x1095_036F,
        12,
        &[c!(&[1], 33), c!(&[2], 20)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopWaterMine",
        0x1E04_04B2,
        0x2D84_05D1,
        12,
        &[c!(&[1], 73), c!(&[2, 3, 4, 7], 29)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopWaterbomb",
        0x1E65_04C9,
        0x2DE5_05E8,
        12,
        &[c!(&[1], 125), c!(&[2, 3, 4], 29)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopWaterbombFly",
        0x2EE3_05F4,
        0x41C0_0713,
        12,
        &[c!(&[1], 77), c!(&[2], 64), c!(&[3, 4], 28), c!(&[5], 20)],
        16,
    ),
    ItemOperationSchema::mapped_u32(
        "GopWaterfly",
        0x19AE_0474,
        0x280F_0593,
        12,
        &[
            c!(&[1], 77),
            c!(&[2], 65),
            c!(&[3], 68),
            c!(&[4], 28),
            c!(&[5], 20),
        ],
        16,
    ),
    ItemOperationSchema::fixed_u32("GopWitchUnionMagic", 0x42B8_070F, 0x58F2_082E, 12, 28),
];

#[must_use]
pub fn item_operation_schema(
    operation_hash: u32,
    base_hash: u32,
) -> Option<&'static ItemOperationSchema> {
    P5136_TYPE12_SCHEMAS
        .iter()
        .find(|schema| schema.pair() == (operation_hash, base_hash))
}

/// Named `Gop*`/`GoItem*` pairs accepted by the C# P5136 relay parser but not
/// represented in the recovered native-writer schema census above.
///
/// The C# source derives its broader set from the checked-in `PacketName` enum
/// rather than from individual packet captures. Keep this small difference set
/// explicit so the Rust parser can use the same family rule without weakening
/// unknown-hash rejection.
const CSHARP_RELAY_ONLY_OPERATION_PAIRS: &[(u32, u32)] = &[
    (0x07AE_0248, 0x1074_0367), // GopEmp/GoItemEmp
    (0x0D69_031A, 0x186D_0439), // GopDevil/GoItemDevil
    (0x0D6A_030E, 0x186E_042D), // GopDrmad/GoItemDrmad
    (0x0D82_031D, 0x1886_043C), // GopJewel/GoItemJewel
    (0x0D8B_032B, 0x188F_044A), // GopGhost/GoItemGhost
    (0x0DAE_0334, 0x18B2_0453), // GopFrost/GoItemFrost
    (0x10CA_034F, 0x1CED_046E), // GopCloud2/GoItemCloud2
    (0x10DE_0382, 0x1D01_04A1), // GopMagnet/GoItemMagnet
    (0x116F_0399, 0x1D92_04B8), // GopSpring/GoItemSpring
    (0x1476_03D8, 0x21B8_04F7), // GopMqDevil/GoItemMqDevil
    (0x14E9_03F7, 0x222B_0516), // GopChopper/GoItemChopper
    (0x18D8_0444, 0x2739_0563), // GopNewDevil/GoItemNewDevil
    (0x1942_0457, 0x27A3_0576), // GopScanning/GoItemScanning
    (0x196B_0451, 0x27CC_0570), // GopSlotLock/GoItemSlotLock
    (0x1DB2_04AF, 0x2D32_05CE), // GopSpeedDown/GoItemSpeedDown
    (0x2271_0505, 0x3310_0624), // GopGoldShield/GoItemGoldShield
    (0x2E54_05E8, 0x4131_0707), // GopSpecialSiren/GoItemSpecialSiren
    (0x3C6F_06D4, 0x518A_07F3), // GopStraightRocket/GoItemStraightRocket
];

/// Returns whether the operation/base pair belongs to the bounded P5136 type-12
/// compatibility family.
#[must_use]
pub fn is_known_item_operation_pair(operation_hash: u32, base_hash: u32) -> bool {
    item_operation_schema(operation_hash, base_hash).is_some()
        || CSHARP_RELAY_ONLY_OPERATION_PAIRS.contains(&(operation_hash, base_hash))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        ItemOperationEvidence, ItemOperationLengthRule, ItemOperationSchema,
        ItemOperationValidationError, P5136_TYPE12_SCHEMA_COUNT, P5136_TYPE12_SCHEMAS,
        item_operation_schema,
    };

    fn raw_for(schema: &ItemOperationSchema, state: u32, length: usize) -> Vec<u8> {
        let mut raw = vec![0; length];
        raw[0..4].copy_from_slice(&schema.operation_hash.to_le_bytes());
        raw[4..8].copy_from_slice(&schema.base_hash.to_le_bytes());
        raw[8..12].copy_from_slice(&1_u32.to_le_bytes());
        match schema.state_field {
            super::ItemOperationStateField::U8 { offset } => {
                raw[offset] = u8::try_from(state).unwrap();
            }
            super::ItemOperationStateField::U32 { offset } => {
                raw[offset..offset + 4].copy_from_slice(&state.to_le_bytes());
            }
        }
        raw
    }

    fn unused_state(schema: &ItemOperationSchema, reserved: Option<u32>) -> u32 {
        (0..=u32::from(u8::MAX))
            .find(|candidate| {
                Some(*candidate) != reserved
                    && match schema.length_rule {
                        ItemOperationLengthRule::StateMap { cases, .. }
                        | ItemOperationLengthRule::CountedState { cases, .. } => {
                            cases.iter().all(|case| !case.states.contains(candidate))
                        }
                        ItemOperationLengthRule::Fixed { .. }
                        | ItemOperationLengthRule::CountedAnyState { .. } => true,
                    }
            })
            .expect("the finite writer state census leaves an unused byte state")
    }

    fn valid_samples(
        schema: &'static ItemOperationSchema,
    ) -> Vec<(Vec<u8>, ItemOperationEvidence)> {
        let mut samples = Vec::new();
        match schema.length_rule {
            ItemOperationLengthRule::Fixed { length, evidence } => {
                samples.push((
                    raw_for(schema, unused_state(schema, None), length),
                    evidence,
                ));
            }
            ItemOperationLengthRule::StateMap {
                cases,
                default_length,
            } => {
                for case in cases {
                    for &state in case.states {
                        samples.push((raw_for(schema, state, case.length), case.evidence));
                    }
                }
                samples.push((
                    raw_for(schema, unused_state(schema, None), default_length),
                    ItemOperationEvidence::StaticWriterDefault,
                ));
            }
            ItemOperationLengthRule::CountedAnyState {
                count_offset,
                base_length,
                element_width,
            } => {
                for state in [0, 2] {
                    let count = 3_u32;
                    let mut raw = raw_for(schema, state, base_length + 3 * element_width);
                    raw[count_offset..count_offset + 4].copy_from_slice(&count.to_le_bytes());
                    let evidence =
                        if schema.class_name == "GopCourse" && matches!(state, 0 | 1) && count == 4
                        {
                            ItemOperationEvidence::RetainedTrace
                        } else {
                            ItemOperationEvidence::StaticWriterDefault
                        };
                    samples.push((raw, evidence));
                }
            }
            ItemOperationLengthRule::CountedState {
                counted_state,
                count_offset,
                base_length,
                element_width,
                cases,
                default_length,
            } => {
                let count = 3_u32;
                let mut counted = raw_for(schema, counted_state, base_length + 3 * element_width);
                counted[count_offset..count_offset + 4].copy_from_slice(&count.to_le_bytes());
                samples.push((counted, ItemOperationEvidence::StaticWriterBranch));
                for case in cases {
                    for &state in case.states {
                        samples.push((raw_for(schema, state, case.length), case.evidence));
                    }
                }
                samples.push((
                    raw_for(
                        schema,
                        unused_state(schema, Some(counted_state)),
                        default_length,
                    ),
                    ItemOperationEvidence::StaticWriterDefault,
                ));
            }
        }
        samples
    }

    #[test]
    fn manifest_has_67_unique_exact_pairs() {
        assert_eq!(P5136_TYPE12_SCHEMAS.len(), P5136_TYPE12_SCHEMA_COUNT);
        let pairs = P5136_TYPE12_SCHEMAS
            .iter()
            .map(|schema| schema.pair())
            .collect::<HashSet<_>>();
        assert_eq!(pairs.len(), P5136_TYPE12_SCHEMA_COUNT);
        for schema in P5136_TYPE12_SCHEMAS {
            assert_eq!(
                item_operation_schema(schema.operation_hash, schema.base_hash),
                Some(schema)
            );
        }
    }

    #[test]
    fn retained_trace_cases_are_distinct_from_static_writer_hypotheses() {
        let banana = item_operation_schema(0x1090_0367, 0x1CB3_0486).unwrap();
        let captured = raw_for(banana, 2, 30);
        assert_eq!(
            banana.validate(&captured).unwrap().evidence,
            ItemOperationEvidence::RetainedTrace
        );
        let static_branch = raw_for(banana, 1, 74);
        assert_eq!(
            banana.validate(&static_branch).unwrap().evidence,
            ItemOperationEvidence::StaticWriterBranch
        );
        let writer_default = raw_for(banana, u32::MAX, 16);
        assert_eq!(
            banana.validate(&writer_default).unwrap().evidence,
            ItemOperationEvidence::StaticWriterDefault
        );

        let barricade = item_operation_schema(0x1D86_04A3, 0x2D06_05C2).unwrap();
        for state in [2, 3] {
            assert_eq!(
                barricade
                    .validate(&raw_for(barricade, state, 25))
                    .unwrap()
                    .evidence,
                ItemOperationEvidence::RetainedTrace
            );
        }
        assert_eq!(
            barricade
                .validate(&raw_for(barricade, 5, 25))
                .unwrap()
                .evidence,
            ItemOperationEvidence::StaticWriterDefault
        );

        // Live two-client P5136 capture: a mine hit is a state-2, 29-byte
        // operation. It must reach the peer that owns the installed object.
        let mine = item_operation_schema(0x0A6B_02AF, 0x1450_03CE).unwrap();
        assert_eq!(
            mine.validate(&raw_for(mine, 2, 29)).unwrap().evidence,
            ItemOperationEvidence::RetainedTrace
        );
    }

    #[test]
    fn rejects_missing_object_ids_and_wrong_state_lengths() {
        let rocket = item_operation_schema(0x1129_038E, 0x1D4C_04AD).unwrap();
        let mut missing = raw_for(rocket, 2, 73);
        missing[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            rocket.validate(&missing),
            Err(ItemOperationValidationError::MissingObjectId {
                class_name: "GopRocket"
            })
        ));

        let wrong = raw_for(rocket, 1, 73);
        assert!(matches!(
            rocket.validate(&wrong),
            Err(ItemOperationValidationError::InvalidLength {
                class_name: "GopRocket",
                state: 1,
                actual: 73,
                expected: 82,
            })
        ));
    }

    #[test]
    fn counted_shapes_are_exact_and_bounded() {
        let course = item_operation_schema(0x1139_0397, 0x0D73_0327).unwrap();
        let mut valid = raw_for(course, 0, 32);
        valid[16..20].copy_from_slice(&4_u32.to_le_bytes());
        assert_eq!(
            course.validate(&valid).unwrap().evidence,
            ItemOperationEvidence::RetainedTrace
        );

        let mut mismatch = valid.clone();
        mismatch[16..20].copy_from_slice(&3_u32.to_le_bytes());
        assert!(matches!(
            course.validate(&mismatch),
            Err(ItemOperationValidationError::InvalidLength {
                class_name: "GopCourse",
                state: 0,
                actual: 32,
                expected: 30,
            })
        ));

        let mut over_cap = raw_for(course, 0, 24);
        over_cap[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            course.validate(&over_cap),
            Err(ItemOperationValidationError::CountOverCap {
                class_name: "GopCourse",
                ..
            })
        ));
    }

    #[test]
    fn every_manifest_branch_and_default_is_exact_and_truncation_safe() {
        for schema in P5136_TYPE12_SCHEMAS {
            let samples = valid_samples(schema);
            assert!(!samples.is_empty(), "{}", schema.class_name);
            for (raw, expected_evidence) in &samples {
                let validated = schema.validate(raw).unwrap_or_else(|error| {
                    panic!("{} valid writer shape failed: {error}", schema.class_name)
                });
                assert_eq!(validated.schema, schema);
                assert_eq!(validated.evidence, *expected_evidence);

                for invalid_length in [raw.len() - 1, raw.len() + 1] {
                    let mut drifted = raw.clone();
                    drifted.resize(invalid_length, 0);
                    assert!(
                        schema.validate(&drifted).is_err(),
                        "{} accepted length {invalid_length} around valid {}",
                        schema.class_name,
                        raw.len()
                    );
                }
            }

            let canonical = &samples[0].0;
            for prefix_length in 0..canonical.len() {
                assert!(
                    schema.validate(&canonical[..prefix_length]).is_err(),
                    "{} accepted truncated prefix {prefix_length}/{}",
                    schema.class_name,
                    canonical.len()
                );
            }

            let mut missing_object = canonical.clone();
            missing_object[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(matches!(
                schema.validate(&missing_object),
                Err(ItemOperationValidationError::MissingObjectId { class_name })
                    if class_name == schema.class_name
            ));
        }
    }
}
