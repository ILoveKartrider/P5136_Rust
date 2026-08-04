//! Class-specific meaning recovered for P5136 `GameSlot` type-12 operations.
//!
//! The native state number is not a protocol-wide lifecycle enum.  Each
//! `GoItem*` consumer interprets it independently.  This module therefore
//! names only transitions supported by producer/consumer evidence and keeps
//! every other class/state explicitly [`ItemLifecycleMeaning::Unknown`].

use crate::game_slot_item_schema::{ItemOperationSchema, ValidatedItemOperation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemSemanticEvidence {
    /// No class-specific producer/consumer meaning has been recovered yet.
    Unresolved,
    /// The `GoItem*` virtual consumer proves the field binding/native phase.
    StaticConsumer,
    /// Both a native producer and the corresponding consumer were recovered.
    ProducerAndConsumer,
    /// Static evidence is also correlated with a retained two-client trace.
    RetainedTraceCorrelated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemLifecycleMeaning {
    Unknown,
    Initialize,
    Place,
    Launch,
    Activate,
    Impact,
    Resolve,
    Retarget,
    Remove,
    RebindSource,
    Respawn,
    /// The packet updates a class-local runtime flag without advancing the
    /// native object lifecycle.
    UpdateRuntimeFlag,
    /// The receiver has an explicit branch which deliberately performs no
    /// class-specific transition.
    NoClientAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemOperationSemantics {
    pub meaning: ItemLifecycleMeaning,
    /// Argument passed to the native `GoItem` phase transition helper.  `None`
    /// means either conditional phases or no phase call, not phase zero.
    pub native_phase: Option<u8>,
    /// ID resolved and bound through the common source/origin-kart helper.
    pub source_object_id: Option<u32>,
    /// ID resolved and bound through the common target-kart (or a recovered
    /// class-specific target) helper.
    pub target_object_id: Option<u32>,
    /// A validated counted target set for operations such as Thunderbolt.
    /// Offsets are relative to the type-12 operation body returned by
    /// `ParsedGameSlotPacket::payload`.
    pub target_object_ids: Option<ItemObjectIdList>,
    /// Per-operation transition/event ID normalized by the client before the
    /// native phase change.
    pub transition_token: Option<u32>,
    /// Class-specific byte retained for diagnostics.  Its value is not treated
    /// as a shared enum across item families.
    pub variant: Option<u8>,
    /// Concrete gameplay item selected by a recovered class-local
    /// discriminator. Most operation classes have no such multiplexing.
    pub effect_item_id: Option<u16>,
    pub evidence: ItemSemanticEvidence,
}

impl ItemOperationSemantics {
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            meaning: ItemLifecycleMeaning::Unknown,
            native_phase: None,
            source_object_id: None,
            target_object_id: None,
            target_object_ids: None,
            transition_token: None,
            variant: None,
            effect_item_id: None,
            evidence: ItemSemanticEvidence::Unresolved,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemObjectIdList {
    raw_offset: usize,
    count: usize,
}

impl ItemObjectIdList {
    #[must_use]
    pub const fn raw_offset(self) -> usize {
        self.raw_offset
    }

    #[must_use]
    pub const fn count(self) -> usize {
        self.count
    }

    /// Decode the already shape-validated little-endian IDs on demand.
    #[must_use]
    pub fn decode(self, raw: &[u8]) -> Option<Vec<u32>> {
        let byte_length = self.count.checked_mul(4)?;
        let end = self.raw_offset.checked_add(byte_length)?;
        raw.get(self.raw_offset..end)?
            .chunks_exact(4)
            .map(|bytes| Some(u32::from_le_bytes(bytes.try_into().ok()?)))
            .collect()
    }
}

/// Decode only meanings proven for an exact, shape-validated class body.
/// `ValidatedItemOperation` has private fields and can only be constructed by
/// the schema validator, so callers cannot skip the length/state gate.
#[must_use]
pub(crate) fn decode_validated_item_operation_semantics(
    validated: ValidatedItemOperation,
    raw: &[u8],
) -> ItemOperationSemantics {
    decode_item_operation_semantics(validated.schema(), raw, validated.state())
}

fn decode_item_operation_semantics(
    schema: &'static ItemOperationSchema,
    raw: &[u8],
    state: u32,
) -> ItemOperationSemantics {
    match schema.class_name {
        "GopAngel" => angel(raw, state),
        "GopBalloon" => balloon(raw, state),
        "GopBanana" => banana(raw, state),
        "GopBarricade" => barricade(raw, state),
        "GopBigTimebomb" => with_effect_item_id(big_timebomb(raw, state), 122),
        "GopBlock" => block(raw, state),
        "GopBossPrison" => boss_prison(raw, state),
        "GopBoundRoad" => bound_road(raw, state),
        "GopBoundWall" => bound_wall(raw, state),
        "GopAreaUfo" => area_ufo(raw, state),
        "GopCokebomb" => bomb(raw, state, BombPhase4Source::Auxiliary),
        "GopCloud" | "GopCloud2" => cloud(schema.class_name, raw, state),
        "GopCourse" => course(raw),
        "GopCube" => cube(raw, state),
        "GopCubeForBoss" => cube_for_boss(raw, state),
        "GopDevil" => devil(raw, state, true, ItemSemanticEvidence::StaticConsumer),
        "GopDynamite" | "GopHammer" => targeted_phase_three_effect(raw, state),
        "GopEmp" => emp(raw, state),
        "GopEventObject" => event_object(raw),
        "GopFalling" => falling(raw, state),
        "GopForceZone" => force_zone(raw, state),
        "GopSnowbomb" => bomb(raw, state, BombPhase4Source::Target),
        "GopGoldShield" => gold_shield(raw, state),
        "GopCokeRocket" | "GopGoldRocket" => {
            rocket_variant(raw, state, RocketVariantFollowup::ThroughState10)
        }
        "GopDinoClawRocket" | "GopTigerRocket" => {
            rocket_variant(raw, state, RocketVariantFollowup::ThroughState9)
        }
        "GopInfectedBomb" => infected_bomb(raw, state),
        "GopIcefly" => with_effect_item_id(icefly(raw, state), 80),
        "GopGiantTalisman" | "GopWitchUnionMagic" => actor_bound_native_phase(raw, state),
        "GopHeadBand" => head_band(raw, state),
        "GopItemTimeFlybomb" | "GopTimeCokebomb" => time_coke_bomb(raw, state),
        "GopLockdownRocket" => lockdown_rocket(raw, state),
        "GopMagnet" => magnet(raw, state),
        "GopMine" => mine(raw, state),
        "GopMovingUfo" => moving_ufo(raw, state),
        "GopMqDevil" => devil(raw, state, true, ItemSemanticEvidence::ProducerAndConsumer),
        "GopNewDevil" => devil(raw, state, false, ItemSemanticEvidence::ProducerAndConsumer),
        "GopOil" => oil(raw, state),
        "GopPiratebomb" => pirate_bomb(raw, state),
        "GopPress" => press(raw, state),
        "GopRobotBeam" => spatial_target_effect(raw, state, 2),
        "GopRollingCokebomb" | "GopRollingbomb" => rolling_bomb(raw, state),
        "GopRollingInfectedbomb" => rolling_infected_bomb(raw, state),
        "GopRocket" => rocket(raw, state),
        "GopScanning" => scanning(raw, state),
        "GopShield" => shield(raw, state),
        "GopSilence" => silence(raw, state),
        "GopSiren" => siren(raw, state),
        "GopSirenShield" => siren_shield(raw, state),
        "GopSlotLock" => slot_lock(raw, state),
        "GopSnowman" => rocket_variant(raw, state, RocketVariantFollowup::ThroughState7),
        "GopSpecialShield" => with_effect_item_id(special_shield(raw, state), 40),
        "GopSpecialSiren" => special_siren(raw, state),
        "GopSpecialSmall" => special_small(raw, state),
        "GopSpaceCraft" => space_craft(raw, state),
        "GopSpeedDown" => speed_down(raw, state),
        "GopStraightRocket" => with_effect_item_id(straight_rocket(raw, state), 73),
        "GopInfectedWaterfly" | "GopSnowWaterfly" | "GopWaterfly" => {
            waterfly(schema.class_name, raw, state)
        }
        "GopSuperMag" => super_magnet(raw, state),
        "GopTargetKart" => target_kart(state),
        "GopTimeMine" => time_mine(raw, state),
        "GopTimeInfectedBomb" => time_infected_bomb(raw, state),
        "GopTimeSnowbomb" => time_snow_bomb(raw, state),
        "GopTimebomb" => with_effect_item_id(time_snow_bomb(raw, state), 13),
        "GopTombStone" => spatial_target_effect(raw, state, 1),
        "GopThunderbolt" => thunderbolt(raw, state),
        "GopUfo" => ufo(raw, state),
        "GopGhost" => ghost(raw, state),
        "GopWaterMine" => water_mine(raw, state),
        "GopWaterbomb" => waterbomb(raw, state),
        "GopWaterbombFly" => waterbomb_fly(raw, state),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `BossPrison` is created by a `GoBossKart` for one selected ordinary kart.
/// State 1 establishes the boss source, target, pose and phase-0 travelling
/// trap. State 2 applies the target-side prison effect without calling the
/// common phase helper. State 3 resolves the prison animation at phase 3;
/// state 4 is the source-rebind/removal path. It advances to phase 4 only when
/// the retained target still resolves; otherwise the client cleans up locally.
/// The compact state-4 writer does not serialize the class-local token read by
/// the consumer.
fn boss_prison(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            None,
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(3),
            None,
            None,
            object_id(raw, 64),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        4 => semantic(
            ItemLifecycleMeaning::Remove,
            None,
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `BombRobot` and `MechanicBall` world controllers place `BoundRoad` hazards from
/// timer-gated three-lane patterns. The follow-up decision byte selects a
/// target-bound phase or local removal; its unaligned trailing source ID is
/// producer-proven but ignored by this concrete consumer.
fn bound_road(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Place,
            Some(0),
            object_id(raw, 53),
            None,
            object_id(raw, 20),
            byte(raw, 62),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 | 3 => contact_hazard_followup(raw, state, 2, 5, None),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `PetitMeteor` and `SpaceBombing` world controllers spawn `Falling` ordnance from
/// timer-gated lane patterns. This is a falling projectile/hazard, not a kart
/// falling out of the course. Unlike `BoundRoad`, its compact consumer also
/// restores the serialized source before binding the hit target.
fn falling(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            object_id(raw, 85),
            None,
            object_id(raw, 20),
            byte(raw, 90),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 | 3 => contact_hazard_followup(raw, state, 3, 5, Some(29)),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn contact_hazard_followup(
    raw: &[u8],
    state: u32,
    state_two_phase: u8,
    state_three_phase: u8,
    source_offset: Option<usize>,
) -> ItemOperationSemantics {
    let has_target = byte(raw, 28).is_some_and(|flag| flag != 0);
    let meaning = if !has_target {
        ItemLifecycleMeaning::Remove
    } else if state == 2 {
        ItemLifecycleMeaning::Impact
    } else {
        ItemLifecycleMeaning::Resolve
    };
    let native_phase = has_target.then_some(if state == 2 {
        state_two_phase
    } else {
        state_three_phase
    });
    semantic(
        meaning,
        native_phase,
        has_target
            .then(|| source_offset.and_then(|offset| object_id(raw, offset)))
            .flatten(),
        has_target.then(|| object_id(raw, 24)).flatten(),
        object_id(raw, 20),
        byte(raw, 28),
        ItemSemanticEvidence::ProducerAndConsumer,
    )
}

/// `Course` carries a kart/event-object ID at raw 12, a counted UTF-16 course
/// event name at raw 20, and a token immediately after the string. Producers
/// use it for the literal `goal` checkpoint/progress notification and `Ev_*`
/// course-script events. The P5136 peer consumer explicitly releases it and
/// performs no client action.
fn course(raw: &[u8]) -> ItemOperationSemantics {
    semantic(
        ItemLifecycleMeaning::NoClientAction,
        None,
        None,
        object_id(raw, 12),
        course_transition_token(raw),
        None,
        ItemSemanticEvidence::ProducerAndConsumer,
    )
}

fn course_transition_token(raw: &[u8]) -> Option<u32> {
    let code_units = usize::try_from(u32_at(raw, 16)?).ok()?;
    let string_bytes = code_units.checked_mul(2)?;
    let token_offset = 20usize.checked_add(string_bytes)?;
    u32_at(raw, token_offset)
}

/// `Piratebomb` is a target-attached timed bomb created for every kart selected
/// by the course/controller predicate. State 2 applies/detonates it at phase
/// 2. State 3 is the explicit cancellation/removal path. State 4 is emitted
/// when an active `SpecialShield` accepts the transition and resolves at phase
/// 4. The state-4 consumer intentionally rebinds both roles to the serialized
/// target ID and ignores the serialized source field.
fn pirate_bomb(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            None,
            object_id(raw, 24),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Remove,
            Some(3),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        4 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(4),
            object_id(raw, 24),
            object_id(raw, 24),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Balloon state 1 advances the receiver to native phase 1 and copies two
/// class-local values. Only the first is exposed as a diagnostic variant;
/// neither value is bound through the common source/target helpers. State 2
/// only sets the receiver's class-local runtime flag.
fn balloon(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(1),
            None,
            None,
            object_id(raw, 16),
            byte(raw, 20),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::UpdateRuntimeFlag,
            None,
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `HeadBand` mirrors Balloon's activation/runtime-flag split. Its state-1
/// byte is a class-local random-effect result and is not an actor identifier.
fn head_band(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(1),
            None,
            None,
            object_id(raw, 16),
            byte(raw, 20),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::UpdateRuntimeFlag,
            None,
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Dynamite and Hammer share an exact writer/consumer contract. State 1
/// activates a source/target-bound effect at phase 0; state 2 publishes the
/// target-bound result at phase 3. Raw 28 is retained as the common class
/// variant. State 1 also has a second class-local byte at raw 29 which is not
/// projected into the one-byte shared semantic view.
fn targeted_phase_three_effect(raw: &[u8], state: u32) -> ItemOperationSemantics {
    let (meaning, native_phase) = match state {
        1 => (ItemLifecycleMeaning::Activate, 0),
        2 => (ItemLifecycleMeaning::Impact, 3),
        _ => return ItemOperationSemantics::unknown(),
    };
    semantic(
        meaning,
        Some(native_phase),
        object_id(raw, 20),
        object_id(raw, 24),
        object_id(raw, 16),
        byte(raw, 28),
        ItemSemanticEvidence::ProducerAndConsumer,
    )
}

/// Press state 1 is a spatial placement. State 2 binds a distinct target and
/// advances the receiver to phase 5.
fn press(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Place,
            Some(0),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(5),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `RobotBeam` and `TombStone` share their spatial writer. State 2 deliberately
/// remains lifecycle-unknown: the producer fills a source member at native
/// offset +84 and the consumer reads it, but the 24-byte writer serializes
/// only token@16 and target@20. Exposing a source@24 would invent wire data.
fn spatial_target_effect(
    raw: &[u8],
    state: u32,
    impact_native_phase: u8,
) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Place,
            Some(0),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Unknown,
            Some(impact_native_phase),
            None,
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `GiantTalisman` and `WitchUnionMagic` serialize the native phase itself at raw
/// 12. The consumer validates both actors and passes that value directly to
/// the phase helper. The exact fields are known, but a gameplay lifecycle
/// label cannot be assigned without knowing the caller-selected phase.
fn actor_bound_native_phase(raw: &[u8], native_phase: u32) -> ItemOperationSemantics {
    semantic(
        ItemLifecycleMeaning::Unknown,
        u8::try_from(native_phase).ok(),
        object_id(raw, 24),
        object_id(raw, 20),
        object_id(raw, 16),
        None,
        ItemSemanticEvidence::ProducerAndConsumer,
    )
}

/// `EventObject` is not a state machine body: raw 12 is the object/kart passed
/// back into the event routine and raw 16 is its normalized event token. Keep
/// it outside the server lifecycle registry while exposing those wire roles.
fn event_object(raw: &[u8]) -> ItemOperationSemantics {
    semantic(
        ItemLifecycleMeaning::Unknown,
        None,
        None,
        object_id(raw, 12),
        object_id(raw, 16),
        None,
        ItemSemanticEvidence::ProducerAndConsumer,
    )
}

/// The only recovered producer emits state 2, and the native receive handler
/// explicitly releases the packet without applying a class transition.
fn target_kart(state: u32) -> ItemOperationSemantics {
    if state == 2 {
        semantic(
            ItemLifecycleMeaning::NoClientAction,
            None,
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        )
    } else {
        ItemOperationSemantics::unknown()
    }
}

/// Block has a class prefix at raw 12 and its state at raw 16. State 2 has a
/// native asymmetry: the consumer reads a source member that the compact
/// writer does not serialize. Only the token, target, flag and selected phase
/// are therefore exposed for that transition.
fn block(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Place,
            Some(0),
            object_id(raw, 24),
            None,
            object_id(raw, 20),
            byte(raw, 88),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => {
            let hit = byte(raw, 28).is_some_and(|flag| flag != 0);
            semantic(
                if hit {
                    ItemLifecycleMeaning::Impact
                } else {
                    ItemLifecycleMeaning::Resolve
                },
                Some(if hit { 3 } else { 4 }),
                None,
                hit.then(|| object_id(raw, 24)).flatten(),
                object_id(raw, 20),
                byte(raw, 28),
                ItemSemanticEvidence::ProducerAndConsumer,
            )
        }
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `BoundWall` also carries a prefix before its state. Compact states 2/3 bind
/// a target only when the serialized presence flag is nonzero; otherwise the
/// receiver performs local cleanup without a native phase call.
fn bound_wall(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Place,
            Some(0),
            object_id(raw, 24),
            None,
            object_id(raw, 20),
            byte(raw, 136),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 | 3 => {
            let has_target = byte(raw, 28).is_some_and(|flag| flag != 0);
            semantic(
                if has_target {
                    if state == 2 {
                        ItemLifecycleMeaning::Impact
                    } else {
                        ItemLifecycleMeaning::Resolve
                    }
                } else {
                    ItemLifecycleMeaning::Remove
                },
                has_target.then(|| u8::try_from(state).ok()).flatten(),
                None,
                has_target.then(|| object_id(raw, 24)).flatten(),
                object_id(raw, 20),
                byte(raw, 28),
                ItemSemanticEvidence::ProducerAndConsumer,
            )
        }
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Cube state 1 carries only the affected target and transition token. The
/// phase-1 call is guarded by the receiver's retained target/phase state, so
/// it is recorded as a target impact with a conditional native phase. State 2
/// carries a vector which the concrete consumer deliberately ignores.
fn cube(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Impact,
            None,
            None,
            object_id(raw, 16),
            object_id(raw, 20),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::NoClientAction,
            None,
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::StaticConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Boss-cube state 0 applies the common transform and enters phase 0. State 1
/// uses the trailing unaligned target/token pair; its large common spatial
/// block is present on the wire but ignored by this consumer branch.
fn cube_for_boss(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        0 => semantic(
            ItemLifecycleMeaning::Place,
            Some(0),
            None,
            None,
            None,
            byte(raw, 68),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        1 => semantic(
            ItemLifecycleMeaning::Impact,
            None,
            None,
            object_id(raw, 69),
            object_id(raw, 73),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Timed team Angel activation and repeatable defense impacts. The shared
/// defense resolver (`sub_99B4B0`) selects state 2 only after an active Angel
/// blocks an attack and returns item id 11. Its trailing `sub_4E83E0` call is a
/// container insertion: callers pass an attack-owned target set as `this` and
/// the protected kart as the inserted value. It does not remove the Angel from
/// the kart's active-effect collection. The state-2 producers create a fresh
/// impact object while requiring the original Angel to remain in active state
/// 1, so successive attacks can be blocked during the effect duration. State 2 carries
/// `token@16, source@20, target@24`; the
/// source-only producer does not explicitly overwrite the target member, so a
/// receiver can bind it only when that serialized ID resolves. The native
/// receiver also normalizes token member +40 but accidentally passes stale
/// state-0 member +28 to phase 2. That client quirk does not make the wire roles
/// or the non-terminal impact lifecycle unknown.
fn angel(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        0 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 21),
            None,
            object_id(raw, 16),
            byte(raw, 20),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Shared timed-shield activation/impact envelope. The native producers use
/// kind 0 for Gold Shield (item 36) and kind 3 for Protect Shield (item 81).
/// Siren Shield reuses the state-2 impact packet and writes item 106 into the
/// trailing `u16`, which takes precedence over the kind discriminator.
/// Activation arms a repeatable timed defense; an impact does not consume it.
fn gold_shield(raw: &[u8], state: u32) -> ItemOperationSemantics {
    let kind_offset = if state == 0 { 24 } else { 28 };
    let Some(kind) = u32_at(raw, kind_offset) else {
        return ItemOperationSemantics::unknown();
    };
    let effect_item_id = if state == 2 && u16_at(raw, 32) == Some(106) {
        106
    } else {
        match kind {
            0 => 36,
            3 => 81,
            _ => return ItemOperationSemantics::unknown(),
        }
    };
    let mut decoded = match state {
        0 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            u8::try_from(kind).ok(),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            u8::try_from(kind).ok(),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => return ItemOperationSemantics::unknown(),
    };
    decoded.effect_item_id = Some(effect_item_id);
    decoded
}

/// EMP is a source-bound phase-0 activation. The two compact bytes are
/// client-local mode flags; only the first is retained as the diagnostic
/// variant and neither is treated as a target identifier.
fn emp(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        0 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 22),
            None,
            object_id(raw, 16),
            byte(raw, 20),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// The retained Icefly implementation is a target-bound phase-0 launch with
/// a full spatial body. The final byte is the class discriminator consumed by
/// the peer; the preceding byte remains a class-local random/effect flag.
fn icefly(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            byte(raw, 77),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Scanning binds the user that activated it and the kart whose slot view is
/// represented by this operation. The producer/consumer pair proves phase 0.
fn scanning(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 16),
            object_id(raw, 24),
            object_id(raw, 20),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `SlotLock` first activates on its source, then publishes a source/target
/// application for each affected kart at native phase 1.
fn slot_lock(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 16),
            None,
            object_id(raw, 24),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(1),
            object_id(raw, 16),
            object_id(raw, 20),
            object_id(raw, 24),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn ghost(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `SpecialSiren` uses a class-specific activation helper rather than the common
/// phase transition call, so its named activation has no fabricated phase.
fn special_siren(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        0 => semantic(
            ItemLifecycleMeaning::Activate,
            None,
            object_id(raw, 21),
            None,
            object_id(raw, 16),
            byte(raw, 20),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// The carrier-style UFO has one source/target launch, three resolution
/// branches, and one class-local runtime flag update. State 4 intentionally
/// ignores the serialized source field, matching the concrete consumer.
fn space_craft(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        0 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            object_id(raw, 24),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 29),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            object_id(raw, 24),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(3),
            object_id(raw, 24),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::StaticConsumer,
        ),
        4 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(6),
            None,
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        5 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(5),
            object_id(raw, 24),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        7 => semantic(
            ItemLifecycleMeaning::UpdateRuntimeFlag,
            None,
            None,
            None,
            None,
            byte(raw, 16),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Only `StraightRocket` state 1 is consumed as a phase-1 launch. The concrete
/// consumer accepts compact states 2 and 3 but performs no class-specific
/// binding, phase transition, or helper call before returning success. They
/// are therefore explicit client no-actions even though no native producer
/// occurrence was recovered for them.
fn straight_rocket(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(1),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            byte(raw, 56),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 | 3 => explicit_no_client_action(),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Both cloud classes share one writer/consumer codec. State 1 installs the
/// spatial cloud, while state 2 names one kart affected by the still-live
/// cloud. The compact state-2 dword is therefore a target, not the install
/// token reused by state 1.
///
/// The state-1 discriminator is also an exact item selector. Native producers
/// map `GopCloud` values 0/3/6 to item IDs 0/1/43 and `GopCloud2` values 0/3/6
/// to item IDs 114/115/116 respectively.
fn cloud(class_name: &str, raw: &[u8], state: u32) -> ItemOperationSemantics {
    let mut decoded = match state {
        1 => semantic(
            ItemLifecycleMeaning::Place,
            Some(0),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            byte(raw, 24),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            None,
            object_id(raw, 16),
            None,
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    };
    if state == 1 {
        decoded.effect_item_id = match (class_name, decoded.variant) {
            ("GopCloud", Some(0)) => Some(0),
            ("GopCloud", Some(3)) => Some(1),
            ("GopCloud", Some(6)) => Some(43),
            ("GopCloud2", Some(0)) => Some(114),
            ("GopCloud2", Some(3)) => Some(115),
            ("GopCloud2", Some(6)) => Some(116),
            _ => None,
        };
    }
    decoded
}

/// Normal magnet binds the user and victim separately before advancing native
/// phase 1. The trailing u16 selects client presentation but is not a shared
/// lifecycle discriminator, so it is intentionally not narrowed into `variant`.
fn magnet(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(1),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `SpeedDown` state 1 binds only the affected kart. The compact state-2 body
/// carries the existing transition token into the client's phase-2 teardown.
fn speed_down(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            None,
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::StaticConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Remove,
            Some(2),
            None,
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::StaticConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `Devil` and `MqDevil` bind a secondary target only for discriminator 5. The
/// `NewDevil` writer omits that target field entirely while preserving the
/// shared token/source activation contract.
fn devil(
    raw: &[u8],
    state: u32,
    has_conditional_target: bool,
    evidence: ItemSemanticEvidence,
) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 21),
            (has_conditional_target && byte(raw, 20) == Some(5))
                .then(|| object_id(raw, 27))
                .flatten(),
            object_id(raw, 16),
            byte(raw, 20),
            evidence,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `ForceZone` uses spatial placement followed by three compact result bodies.
/// The success byte at raw 24 controls whether the client can bind actors and
/// enter phases 2/3/5. Failed results select either phase 4 or local teardown
/// from a client-runtime flag, so Rust keeps them nonterminal with no claimed
/// native phase.
fn force_zone(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Place,
            Some(0),
            object_id(raw, 68),
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => {
            let succeeded = byte(raw, 24).is_some_and(|flag| flag != 0);
            semantic(
                if succeeded {
                    ItemLifecycleMeaning::Impact
                } else {
                    ItemLifecycleMeaning::Resolve
                },
                succeeded.then_some(2),
                object_id(raw, 25),
                succeeded.then(|| object_id(raw, 20)).flatten(),
                object_id(raw, 16),
                byte(raw, 24),
                ItemSemanticEvidence::ProducerAndConsumer,
            )
        }
        3 => {
            let succeeded = byte(raw, 24).is_some_and(|flag| flag != 0);
            semantic(
                ItemLifecycleMeaning::Resolve,
                succeeded.then_some(3),
                succeeded.then(|| object_id(raw, 20)).flatten(),
                succeeded.then(|| object_id(raw, 25)).flatten(),
                object_id(raw, 16),
                byte(raw, 24),
                ItemSemanticEvidence::ProducerAndConsumer,
            )
        }
        5 => {
            let succeeded = byte(raw, 24).is_some_and(|flag| flag != 0);
            semantic(
                ItemLifecycleMeaning::Resolve,
                succeeded.then_some(5),
                succeeded.then(|| object_id(raw, 20)).flatten(),
                None,
                // Unlike states 1-3, the receiver forwards raw 16 without
                // normalizing it. It therefore is not a transition_token.
                None,
                byte(raw, 24),
                ItemSemanticEvidence::ProducerAndConsumer,
            )
        }
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `Oil` mirrors `ForceZone`'s placement/result split but has no state-5 branch.
fn oil(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Place,
            Some(0),
            object_id(raw, 69),
            None,
            object_id(raw, 16),
            byte(raw, 20),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => {
            let succeeded = byte(raw, 24).is_some_and(|flag| flag != 0);
            semantic(
                if succeeded {
                    ItemLifecycleMeaning::Impact
                } else {
                    ItemLifecycleMeaning::Remove
                },
                succeeded.then_some(2),
                object_id(raw, 25),
                succeeded.then(|| object_id(raw, 20)).flatten(),
                object_id(raw, 16),
                byte(raw, 24),
                ItemSemanticEvidence::ProducerAndConsumer,
            )
        }
        3 => {
            let succeeded = byte(raw, 24).is_some_and(|flag| flag != 0);
            semantic(
                if succeeded {
                    ItemLifecycleMeaning::Resolve
                } else {
                    ItemLifecycleMeaning::Remove
                },
                succeeded.then_some(3),
                succeeded.then(|| object_id(raw, 20)).flatten(),
                None,
                object_id(raw, 16),
                byte(raw, 24),
                ItemSemanticEvidence::StaticConsumer,
            )
        }
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Silence state 2 has the same serialized actor body as state 1, but the
/// recovered receiver intentionally has no state-2 action.
fn silence(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 21),
            object_id(raw, 25),
            object_id(raw, 17),
            byte(raw, 16),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::NoClientAction,
            None,
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::StaticConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn siren(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 21),
            None,
            object_id(raw, 16),
            byte(raw, 20),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(1),
            object_id(raw, 24),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn siren_shield(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        0 | 2 => semantic(
            if state == 0 {
                ItemLifecycleMeaning::Activate
            } else {
                ItemLifecycleMeaning::Resolve
            },
            u8::try_from(state).ok(),
            object_id(raw, 21),
            None,
            object_id(raw, 16),
            byte(raw, 20),
            if state == 0 {
                ItemSemanticEvidence::ProducerAndConsumer
            } else {
                ItemSemanticEvidence::StaticConsumer
            },
        ),
        1 => {
            let actor = object_id(raw, 20);
            semantic(
                ItemLifecycleMeaning::Impact,
                Some(1),
                actor,
                actor,
                object_id(raw, 16),
                None,
                ItemSemanticEvidence::StaticConsumer,
            )
        }
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `SpecialSmall` state 2 only copies raw 16 into the client object's runtime
/// flag at offset 288; it does not bind actors or invoke a native phase.
fn special_small(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        0 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 24),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 29),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        1 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(3),
            object_id(raw, 24),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::UpdateRuntimeFlag,
            None,
            None,
            None,
            None,
            byte(raw, 16),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RocketVariantFollowup {
    ThroughState7,
    ThroughState9,
    ThroughState10,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BombPhase4Source {
    Auxiliary,
    Target,
}

const fn semantic(
    meaning: ItemLifecycleMeaning,
    native_phase: Option<u8>,
    source_object_id: Option<u32>,
    target_object_id: Option<u32>,
    transition_token: Option<u32>,
    variant: Option<u8>,
    evidence: ItemSemanticEvidence,
) -> ItemOperationSemantics {
    ItemOperationSemantics {
        meaning,
        native_phase,
        source_object_id,
        target_object_id,
        target_object_ids: None,
        transition_token,
        variant,
        effect_item_id: None,
        evidence,
    }
}

fn barricade(raw: &[u8], state: u32) -> ItemOperationSemantics {
    let evidence = if state == 2 {
        ItemSemanticEvidence::RetainedTraceCorrelated
    } else {
        ItemSemanticEvidence::ProducerAndConsumer
    };
    let (meaning, native_phase, variant) = match state {
        0 => (ItemLifecycleMeaning::Initialize, Some(0), None),
        1 => (ItemLifecycleMeaning::Place, Some(1), None),
        2 => (ItemLifecycleMeaning::Impact, Some(3), None),
        3 => (ItemLifecycleMeaning::Resolve, Some(4), None),
        // The variant byte selects native phase 5 or 6.
        4 => (ItemLifecycleMeaning::Remove, None, byte(raw, 25)),
        _ => return ItemOperationSemantics::unknown(),
    };
    semantic(
        meaning,
        native_phase,
        object_id(raw, 17),
        object_id(raw, 21),
        object_id(raw, 13),
        variant,
        evidence,
    )
}

fn banana(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Place,
            Some(0),
            object_id(raw, 69),
            None,
            object_id(raw, 16),
            byte(raw, 20),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            None,
            object_id(raw, 25),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 24),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Resolve,
            None,
            object_id(raw, 20),
            object_id(raw, 25),
            object_id(raw, 16),
            byte(raw, 24),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `GopBigTimebomb` does not carry the ordinary class lifecycle state at raw
/// offset 12. The receiver binds both actors and forwards the dword at 13
/// directly to the native phase helper. Producer call sites constrain that
/// dword to activation phase 0, ordinary/team-routed impact phases 2/3, and
/// `SpecialShield` resolution phase 4. The client performs every binding and
/// phase/token/variant operation only after both actor lookups succeed, so all
/// conditional semantics remain absent when either serialized actor ID is
/// missing.
fn big_timebomb(raw: &[u8], native_phase: u32) -> ItemOperationSemantics {
    let source = object_id(raw, 25);
    let target = object_id(raw, 21);
    let Some((source, target)) = source.zip(target) else {
        return ItemOperationSemantics {
            meaning: ItemLifecycleMeaning::Unknown,
            evidence: ItemSemanticEvidence::ProducerAndConsumer,
            ..ItemOperationSemantics::unknown()
        };
    };

    let meaning = match native_phase {
        0 => ItemLifecycleMeaning::Activate,
        2 | 3 => ItemLifecycleMeaning::Impact,
        4 => ItemLifecycleMeaning::Resolve,
        _ => ItemLifecycleMeaning::Unknown,
    };
    semantic(
        meaning,
        u8::try_from(native_phase).ok(),
        Some(source),
        Some(target),
        object_id(raw, 17),
        byte(raw, 12),
        ItemSemanticEvidence::ProducerAndConsumer,
    )
}

/// Area-UFO state 1 binds two distinct actors and preserves an additional
/// class dword at raw 29 that is not a shared lifecycle field. State 2 binds
/// raw 20 as both actors before advancing to phase 5.
fn area_ufo(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 25),
            object_id(raw, 21),
            object_id(raw, 16),
            byte(raw, 20),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => {
            let actor = object_id(raw, 20);
            semantic(
                ItemLifecycleMeaning::Resolve,
                Some(5),
                actor,
                actor,
                object_id(raw, 16),
                None,
                ItemSemanticEvidence::ProducerAndConsumer,
            )
        }
        _ => ItemOperationSemantics::unknown(),
    }
}

/// `LockdownRocket` carries a byte state and then uses a different compact
/// layout for nearly every transition. State 3 is the receiver's explicit
/// teardown branch. States 5/6 have a serialized phase selector but also
/// require an already-bound runtime target, so their phase remains conditional.
fn lockdown_rocket(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            None,
            None,
            object_id(raw, 14),
            byte(raw, 13),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Retarget,
            None,
            None,
            object_id(raw, 13),
            None,
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Remove,
            None,
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::StaticConsumer,
        ),
        4 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(1),
            object_id(raw, 17),
            object_id(raw, 21),
            object_id(raw, 13),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        5 | 6 => semantic(
            ItemLifecycleMeaning::Resolve,
            None,
            None,
            None,
            object_id(raw, 13),
            byte(raw, 17),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        7 => semantic(
            ItemLifecycleMeaning::Resolve,
            byte(raw, 25).map(|variant| if variant == 0 { 7 } else { 8 }),
            object_id(raw, 17),
            object_id(raw, 21),
            object_id(raw, 13),
            byte(raw, 25),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        8 => semantic(
            ItemLifecycleMeaning::Resolve,
            byte(raw, 25).map(|variant| if variant == 0 { 11 } else { 10 }),
            object_id(raw, 17),
            object_id(raw, 21),
            object_id(raw, 13),
            byte(raw, 25),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        9 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(9),
            object_id(raw, 17),
            object_id(raw, 21),
            object_id(raw, 13),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Ordinary Shield follows the same non-terminal defense-hit pattern. Its
/// state-1 body starts with `item_id:u16@16`, followed by an unaligned
/// transition token at raw offset 18. The native producer selects ordinary
/// Shield (10), Super Shield (18), or the shield half of Super Magnet (103)
/// through this item ID. Its state-2 producer constructs a separate hit object
/// and does not call the active Shield's cleanup path.
fn shield(raw: &[u8], state: u32) -> ItemOperationSemantics {
    let mut decoded = match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 22),
            None,
            object_id(raw, 18),
            byte(raw, 30),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(1),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    };
    if state == 1 {
        decoded.effect_item_id = u16_at(raw, 16);
    }
    decoded
}

/// `SpecialShield` has a one-byte item discriminator at raw 16 before its
/// unaligned token. The state-0 body contains two further class-specific bytes;
/// they are deliberately not promoted into a protocol-wide enum.
fn special_shield(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        0 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 22),
            None,
            object_id(raw, 17),
            byte(raw, 16),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 | 3 => semantic(
            if state == 2 {
                ItemLifecycleMeaning::Impact
            } else {
                ItemLifecycleMeaning::Resolve
            },
            u8::try_from(state).ok(),
            object_id(raw, 21),
            None,
            object_id(raw, 17),
            byte(raw, 16),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn moving_ufo(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            // The receiver advances to phase 2 and binds raw 20 only when it
            // does not already have a runtime target.
            None,
            None,
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Ordinary UFO shares `AreaUfo`'s state-1 actor layout. Its compact state-2
/// notification only updates existing runtime state (or tears the item down),
/// and does not unconditionally normalize a token or advance a native phase.
fn ufo(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 25),
            object_id(raw, 21),
            object_id(raw, 16),
            byte(raw, 20),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Resolve,
            None,
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Thunderbolt state 1 carries a counted set of affected kart IDs. States 2
/// and 3 are two target-impact branches guarded by successful target lookup.
fn thunderbolt(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => {
            let mut decoded = semantic(
                ItemLifecycleMeaning::Activate,
                Some(0),
                object_id(raw, 21),
                None,
                object_id(raw, 16),
                raw.last().copied(),
                ItemSemanticEvidence::ProducerAndConsumer,
            );
            decoded.target_object_ids = object_id_list(raw, 25, 29);
            decoded
        }
        2 | 3 => {
            let target = object_id(raw, 21);
            if target.is_none() {
                return semantic(
                    ItemLifecycleMeaning::Impact,
                    None,
                    None,
                    None,
                    None,
                    None,
                    ItemSemanticEvidence::ProducerAndConsumer,
                );
            }
            semantic(
                ItemLifecycleMeaning::Impact,
                Some(if state == 2 { 4 } else { 3 }),
                (state == 3).then(|| object_id(raw, 25)).flatten(),
                target,
                object_id(raw, 17),
                None,
                ItemSemanticEvidence::ProducerAndConsumer,
            )
        }
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Shared Cokebomb/Snowbomb contract. The compact state-3 consumer binds the
/// same serialized kart as both target and source. Cokebomb separately uses
/// raw offset 24 as an effect kart; Snowbomb does not consume that field.
fn bomb(raw: &[u8], state: u32, phase4_source: BombPhase4Source) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            object_id(raw, 24),
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(3),
            object_id(raw, 20),
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        4 => semantic(
            // Phase 4 is a cleanup candidate, but no teardown call or retained
            // peer trace yet proves registry-terminal removal.
            ItemLifecycleMeaning::Resolve,
            Some(4),
            object_id(
                raw,
                match phase4_source {
                    BombPhase4Source::Auxiliary => 24,
                    BombPhase4Source::Target => 20,
                },
            ),
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn infected_bomb(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            byte(raw, 120),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            object_id(raw, 29),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(4),
            object_id(raw, 29),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn rolling_bomb(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            object_id(raw, 24),
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(3),
            object_id(raw, 20),
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        4 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(4),
            object_id(raw, 20),
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn rolling_infected_bomb(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            object_id(raw, 28),
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(4),
            None,
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Timed flying/coke bomb family.  The compact state-3/4 bodies bind their
/// sole actor ID as both source and target, matching the client consumer.
fn time_coke_bomb(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            object_id(raw, 24),
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 | 4 => semantic(
            ItemLifecycleMeaning::Resolve,
            u8::try_from(state).ok(),
            object_id(raw, 20),
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// The infected timed bomb consumes an additional dword at raw offset 24 via
/// a class-specific helper.  Its name remains unknown, so it is deliberately
/// not squeezed into the byte-sized `variant` diagnostic field.
fn time_infected_bomb(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            object_id(raw, 28),
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(4),
            None,
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

/// Timebomb/Snowbomb state 2 consumes the serialized source at raw offset 24;
/// states 3/4 still serialize that dword but the peer ignores it and binds the
/// actor at raw offset 20 as both source and target.
fn time_snow_bomb(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            object_id(raw, 24),
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 | 4 => semantic(
            ItemLifecycleMeaning::Resolve,
            u8::try_from(state).ok(),
            object_id(raw, 20),
            object_id(raw, 20),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn mine(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Place,
            Some(0),
            object_id(raw, 73),
            None,
            object_id(raw, 16),
            byte(raw, 72),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            None,
            object_id(raw, 25),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 24),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::NoClientAction,
            None,
            None,
            None,
            object_id(raw, 16),
            byte(raw, 24),
            ItemSemanticEvidence::StaticConsumer,
        ),
        4 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(4),
            object_id(raw, 20),
            object_id(raw, 25),
            object_id(raw, 16),
            byte(raw, 24),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        5 => semantic(
            ItemLifecycleMeaning::Remove,
            None,
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            byte(raw, 24),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        6 => semantic(
            ItemLifecycleMeaning::Respawn,
            Some(0),
            None,
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn water_mine(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Place,
            Some(0),
            object_id(raw, 68),
            None,
            object_id(raw, 16),
            byte(raw, 72),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            object_id(raw, 24),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(3),
            object_id(raw, 24),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        4 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(4),
            object_id(raw, 24),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        // The writer emits this producer-side phase notification, while the
        // peer handler has no state-7 branch.
        7 => explicit_no_client_action(),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn time_mine(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Place,
            Some(0),
            object_id(raw, 81),
            None,
            object_id(raw, 20),
            byte(raw, 80),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => {
            let has_target_branch = byte(raw, 28).is_some_and(|flag| flag != 0);
            semantic(
                if has_target_branch {
                    ItemLifecycleMeaning::Impact
                } else {
                    ItemLifecycleMeaning::Resolve
                },
                // Even with the serialized flag, existing client runtime
                // target/reset state can suppress the phase-2 call.
                None,
                None,
                has_target_branch.then(|| object_id(raw, 24)).flatten(),
                object_id(raw, 20),
                byte(raw, 28),
                ItemSemanticEvidence::ProducerAndConsumer,
            )
        }
        // State 3 advances to the distinct phase-5 post-impact path. State 5
        // is a later phase-7 transition; neither is called registry-terminal
        // until teardown or retained-trace evidence proves that boundary.
        3 => semantic(
            ItemLifecycleMeaning::Resolve,
            None,
            None,
            byte(raw, 28)
                .is_some_and(|flag| flag != 0)
                .then(|| object_id(raw, 24))
                .flatten(),
            object_id(raw, 20),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        4 => explicit_no_client_action(),
        5 => semantic(
            ItemLifecycleMeaning::Resolve,
            None,
            None,
            None,
            object_id(raw, 20),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn rocket(raw: &[u8], state: u32) -> ItemOperationSemantics {
    let mut decoded = match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(1),
            object_id(raw, 22),
            object_id(raw, 75),
            object_id(raw, 18),
            byte(raw, 26),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            object_id(raw, 68),
            None,
            object_id(raw, 16),
            byte(raw, 72),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(3),
            None,
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        4 => semantic(
            ItemLifecycleMeaning::Retarget,
            None,
            None,
            object_id(raw, 16),
            None,
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        5 => semantic(
            ItemLifecycleMeaning::Remove,
            None,
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        6 => semantic(
            ItemLifecycleMeaning::NoClientAction,
            None,
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::StaticConsumer,
        ),
        7 => semantic(
            ItemLifecycleMeaning::RebindSource,
            Some(6),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        8..=10 => semantic(
            ItemLifecycleMeaning::Resolve,
            u8::try_from(state - 1).ok(),
            None,
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    };
    if state == 1 {
        // The common Rocket writer serializes the concrete item selector
        // before its unaligned transition token. This distinguishes normal
        // Rocket (7) from first-place Guide Rocket (33) without a new Gop
        // class.
        decoded.effect_item_id = u16_at(raw, 16);
    }
    decoded
}

fn rocket_variant(
    raw: &[u8],
    state: u32,
    followup: RocketVariantFollowup,
) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(1),
            object_id(raw, 20),
            object_id(raw, 73),
            object_id(raw, 16),
            byte(raw, 24),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            object_id(raw, 68),
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(3),
            None,
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        4 => semantic(
            ItemLifecycleMeaning::Retarget,
            None,
            None,
            object_id(raw, 16),
            None,
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        5 => semantic(
            ItemLifecycleMeaning::Remove,
            None,
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        7 => semantic(
            ItemLifecycleMeaning::RebindSource,
            Some(6),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        8 if followup != RocketVariantFollowup::ThroughState7 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(7),
            None,
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        9 if followup != RocketVariantFollowup::ThroughState7 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(8),
            None,
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        10 if followup == RocketVariantFollowup::ThroughState10 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(9),
            None,
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        6 | 8..=10 => explicit_no_client_action(),
        _ => ItemOperationSemantics::unknown(),
    }
}

const fn explicit_no_client_action() -> ItemOperationSemantics {
    semantic(
        ItemLifecycleMeaning::NoClientAction,
        None,
        None,
        None,
        None,
        None,
        ItemSemanticEvidence::StaticConsumer,
    )
}

fn super_magnet(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Activate,
            Some(0),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        // The receiver has a phase-4 branch, but the native writer does not
        // serialize its actor fields for state 2.
        2 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(4),
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::StaticConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn waterbomb(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            object_id(raw, 20),
            None,
            object_id(raw, 16),
            byte(raw, 120),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 | 3 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(if state == 2 { 2 } else { 3 }),
            object_id(raw, 25),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 24),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        4 => semantic(
            ItemLifecycleMeaning::Remove,
            Some(4),
            object_id(raw, 25),
            object_id(raw, 20),
            object_id(raw, 16),
            byte(raw, 24),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn waterbomb_fly(raw: &[u8], state: u32) -> ItemOperationSemantics {
    match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            None,
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(3),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        4 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(4),
            object_id(raw, 20),
            object_id(raw, 24),
            None,
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        5 => explicit_no_client_action(),
        // The native writer has no state-6 body, but the consumer has the
        // terminal phase-5 branch. Do not invent the untransmitted token.
        6 => semantic(
            ItemLifecycleMeaning::Remove,
            None,
            None,
            None,
            None,
            None,
            ItemSemanticEvidence::StaticConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    }
}

fn waterfly(class_name: &str, raw: &[u8], state: u32) -> ItemOperationSemantics {
    let decoded = match state {
        1 => semantic(
            ItemLifecycleMeaning::Launch,
            Some(0),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            byte(raw, 28),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        2 => semantic(
            ItemLifecycleMeaning::Impact,
            None,
            None,
            None,
            None,
            byte(raw, 64),
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        3 => semantic(
            ItemLifecycleMeaning::Impact,
            Some(2),
            None,
            None,
            object_id(raw, 64),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        4 => semantic(
            ItemLifecycleMeaning::Resolve,
            Some(3),
            object_id(raw, 20),
            object_id(raw, 24),
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        5 => semantic(
            ItemLifecycleMeaning::Remove,
            Some(4),
            None,
            None,
            object_id(raw, 16),
            None,
            ItemSemanticEvidence::ProducerAndConsumer,
        ),
        _ => ItemOperationSemantics::unknown(),
    };
    match class_name {
        "GopWaterfly" => with_effect_item_id(decoded, 4),
        "GopSnowWaterfly" => with_effect_item_id(decoded, 118),
        "GopInfectedWaterfly" => with_effect_item_id(decoded, 119),
        _ => decoded,
    }
}

fn object_id(raw: &[u8], offset: usize) -> Option<u32> {
    let value = u32_at(raw, offset)?;
    (value != u32::MAX).then_some(value)
}

fn object_id_list(raw: &[u8], count_offset: usize, raw_offset: usize) -> Option<ItemObjectIdList> {
    let count = usize::try_from(u32_at(raw, count_offset)?).ok()?;
    let byte_length = count.checked_mul(4)?;
    let end = raw_offset.checked_add(byte_length)?;
    raw.get(raw_offset..end)?;
    Some(ItemObjectIdList { raw_offset, count })
}

fn u32_at(raw: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        raw.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn u16_at(raw: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        raw.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn byte(raw: &[u8], offset: usize) -> Option<u8> {
    raw.get(offset).copied()
}

const fn with_effect_item_id(
    mut semantics: ItemOperationSemantics,
    effect_item_id: u16,
) -> ItemOperationSemantics {
    semantics.effect_item_id = Some(effect_item_id);
    semantics
}

#[cfg(test)]
mod tests {
    use crate::game_slot_item_schema::item_operation_schema;

    use super::{ItemLifecycleMeaning, ItemSemanticEvidence, decode_item_operation_semantics};

    fn schema(pair: (u32, u32)) -> &'static crate::game_slot_item_schema::ItemOperationSchema {
        item_operation_schema(pair.0, pair.1).expect("test pair has a checked-in schema")
    }

    fn put_u32(raw: &mut [u8], offset: usize, value: u32) {
        raw[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn recovered_item_selectors_keep_exact_class_and_byte_meanings() {
        let shield = schema((0x1110_037F, 0x1D33_049E));
        let mut raw = vec![0; 31];
        raw[16..18].copy_from_slice(&18_u16.to_le_bytes());
        put_u32(&mut raw, 18, 0x7100_0001);
        put_u32(&mut raw, 22, 0x7200_0002);
        raw[30] = 9;
        let decoded = decode_item_operation_semantics(shield, &raw, 1);
        assert_eq!(decoded.effect_item_id, Some(18));
        assert_eq!(decoded.transition_token, Some(0x7100_0001));
        assert_eq!(decoded.source_object_id, Some(0x7200_0002));
        assert_eq!(decoded.variant, Some(9));

        let rocket = schema((0x1129_038E, 0x1D4C_04AD));
        let mut raw = vec![0; 82];
        raw[16..18].copy_from_slice(&33_u16.to_le_bytes());
        put_u32(&mut raw, 18, 0x7300_0003);
        let decoded = decode_item_operation_semantics(rocket, &raw, 1);
        assert_eq!(decoded.effect_item_id, Some(33));
        assert_eq!(decoded.transition_token, Some(0x7300_0003));

        for (pair, discriminator, item_id) in [
            ((0x0D7B_031D, 0x187F_043C), 0, 0),
            ((0x0D7B_031D, 0x187F_043C), 3, 1),
            ((0x0D7B_031D, 0x187F_043C), 6, 43),
            ((0x10CA_034F, 0x1CED_046E), 0, 114),
            ((0x10CA_034F, 0x1CED_046E), 3, 115),
            ((0x10CA_034F, 0x1CED_046E), 6, 116),
        ] {
            let mut raw = vec![0; 73];
            raw[24] = discriminator;
            let decoded = decode_item_operation_semantics(schema(pair), &raw, 1);
            assert_eq!(decoded.effect_item_id, Some(item_id));
        }

        for (pair, state, length, item_id) in [
            ((0x276B_0567, 0x3929_0686), 0, 29, 122),
            ((0x10C3_0382, 0x1CE6_04A1), 1, 78, 80),
            ((0x3473_0640, 0x486F_075F), 0, 27, 40),
            ((0x3C6F_06D4, 0x518A_07F3), 1, 58, 73),
            ((0x196A_0455, 0x27CB_0574), 1, 24, 13),
            ((0x2F69_061B, 0x4246_073A), 1, 77, 118),
        ] {
            let decoded = decode_item_operation_semantics(schema(pair), &vec![0; length], state);
            assert_eq!(decoded.effect_item_id, Some(item_id));
        }
    }

    #[test]
    fn super_magnet_state_one_has_distinct_transition_source_and_target() {
        let mut raw = vec![0; 29];
        put_u32(&mut raw, 16, 0x1111_1111);
        put_u32(&mut raw, 20, 0x2222_2222);
        put_u32(&mut raw, 24, 0x3333_3333);
        raw[28] = 1;
        let decoded = decode_item_operation_semantics(schema((0x198F_044A, 0x27F0_0569)), &raw, 1);
        assert_eq!(decoded.meaning, ItemLifecycleMeaning::Activate);
        assert_eq!(decoded.transition_token, Some(0x1111_1111));
        assert_eq!(decoded.source_object_id, Some(0x2222_2222));
        assert_eq!(decoded.target_object_id, Some(0x3333_3333));
        assert_eq!(decoded.variant, Some(1));
    }

    #[test]
    fn compact_waterbomb_transition_does_not_swap_source_and_target() {
        let mut raw = vec![0; 29];
        put_u32(&mut raw, 16, 0xAAAA_0001);
        put_u32(&mut raw, 20, 0xBBBB_0002);
        raw[24] = 7;
        put_u32(&mut raw, 25, 0xCCCC_0003);
        let decoded = decode_item_operation_semantics(schema((0x1E65_04C9, 0x2DE5_05E8)), &raw, 2);
        assert_eq!(decoded.meaning, ItemLifecycleMeaning::Impact);
        assert_eq!(decoded.native_phase, Some(2));
        assert_eq!(decoded.transition_token, Some(0xAAAA_0001));
        assert_eq!(decoded.target_object_id, Some(0xBBBB_0002));
        assert_eq!(decoded.source_object_id, Some(0xCCCC_0003));
    }

    #[test]
    fn barricade_state_two_is_trace_correlated_impact_but_state_three_is_resolve() {
        let schema = schema((0x1D86_04A3, 0x2D06_05C2));
        let raw = vec![0; 25];
        let hit = decode_item_operation_semantics(schema, &raw, 2);
        let resolve = decode_item_operation_semantics(schema, &raw, 3);
        assert_eq!(hit.meaning, ItemLifecycleMeaning::Impact);
        assert_eq!(hit.native_phase, Some(3));
        assert_eq!(hit.evidence, ItemSemanticEvidence::RetainedTraceCorrelated);
        assert_eq!(resolve.meaning, ItemLifecycleMeaning::Resolve);
        assert_eq!(resolve.native_phase, Some(4));
    }

    #[test]
    fn recovered_course_hazard_and_pirate_consumers_preserve_actor_roles() {
        let boss = schema((0x233A_0538, 0x33D9_0657));
        let mut boss_launch = vec![0; 77];
        put_u32(&mut boss_launch, 16, 0xB001);
        put_u32(&mut boss_launch, 20, 0xB002);
        put_u32(&mut boss_launch, 24, 0xB003);
        boss_launch[28] = 7;
        let decoded = decode_item_operation_semantics(boss, &boss_launch, 1);
        assert_eq!(decoded.meaning, ItemLifecycleMeaning::Launch);
        assert_eq!(decoded.native_phase, Some(0));
        assert_eq!(decoded.transition_token, Some(0xB001));
        assert_eq!(decoded.source_object_id, Some(0xB002));
        assert_eq!(decoded.target_object_id, Some(0xB003));
        assert_eq!(decoded.variant, Some(7));
        assert_eq!(decoded.evidence, ItemSemanticEvidence::ProducerAndConsumer);

        let mut boss_resolve = vec![0; 68];
        put_u32(&mut boss_resolve, 64, 0xB004);
        let decoded = decode_item_operation_semantics(boss, &boss_resolve, 3);
        assert_eq!(decoded.meaning, ItemLifecycleMeaning::Resolve);
        assert_eq!(decoded.native_phase, Some(3));
        assert_eq!(decoded.transition_token, Some(0xB004));
        let decoded = decode_item_operation_semantics(boss, &[0; 16], 4);
        assert_eq!(decoded.meaning, ItemLifecycleMeaning::Remove);
        assert_eq!(decoded.native_phase, None);

        for (pair, source_offset, launch_length, launch_meaning) in [
            (
                (0x1DB9_04A4, 0x2D39_05C3),
                53,
                63,
                ItemLifecycleMeaning::Place,
            ),
            (
                (0x14A7_03E3, 0x21E9_0502),
                85,
                91,
                ItemLifecycleMeaning::Launch,
            ),
        ] {
            let schema = schema(pair);
            let mut launch = vec![0; launch_length];
            put_u32(&mut launch, 20, 0xC001);
            put_u32(&mut launch, source_offset, 0xC002);
            launch[launch_length - 1] = 9;
            let decoded = decode_item_operation_semantics(schema, &launch, 1);
            assert_eq!(decoded.meaning, launch_meaning);
            assert_eq!(decoded.native_phase, Some(0));
            assert_eq!(decoded.transition_token, Some(0xC001));
            assert_eq!(decoded.source_object_id, Some(0xC002));
            assert_eq!(decoded.variant, Some(9));

            let mut impact = vec![0; 33];
            put_u32(&mut impact, 20, 0xC003);
            put_u32(&mut impact, 24, 0xC004);
            impact[28] = 1;
            put_u32(&mut impact, 29, 0xC005);
            let decoded = decode_item_operation_semantics(schema, &impact, 2);
            assert_eq!(decoded.meaning, ItemLifecycleMeaning::Impact);
            assert_eq!(decoded.transition_token, Some(0xC003));
            assert_eq!(decoded.target_object_id, Some(0xC004));
            assert_eq!(
                decoded.source_object_id,
                (source_offset == 85).then_some(0xC005)
            );

            impact[28] = 0;
            let decoded = decode_item_operation_semantics(schema, &impact, 3);
            assert_eq!(decoded.meaning, ItemLifecycleMeaning::Remove);
            assert_eq!(decoded.native_phase, None);
            assert_eq!(decoded.source_object_id, None);
            assert_eq!(decoded.target_object_id, None);
        }

        let course = schema((0x1139_0397, 0x0D73_0327));
        let mut goal = vec![0; 32];
        put_u32(&mut goal, 12, 0xD001);
        put_u32(&mut goal, 16, 4);
        goal[20..28].copy_from_slice(&[b'g', 0, b'o', 0, b'a', 0, b'l', 0]);
        put_u32(&mut goal, 28, 0xD002);
        let decoded = decode_item_operation_semantics(course, &goal, 0xD001);
        assert_eq!(decoded.meaning, ItemLifecycleMeaning::NoClientAction);
        assert_eq!(decoded.target_object_id, Some(0xD001));
        assert_eq!(decoded.transition_token, Some(0xD002));

        let pirate = schema((0x2369_052B, 0x3408_064A));
        let mut raw = vec![0; 28];
        put_u32(&mut raw, 16, 0xE001);
        put_u32(&mut raw, 20, 0xE002);
        put_u32(&mut raw, 24, 0xE003);
        for (state, meaning, phase) in [
            (1, ItemLifecycleMeaning::Activate, 0),
            (2, ItemLifecycleMeaning::Impact, 2),
            (3, ItemLifecycleMeaning::Remove, 3),
            (4, ItemLifecycleMeaning::Resolve, 4),
        ] {
            let decoded = decode_item_operation_semantics(pirate, &raw, state);
            assert_eq!(decoded.meaning, meaning);
            assert_eq!(decoded.native_phase, Some(phase));
            assert_eq!(decoded.transition_token, Some(0xE001));
        }
        let shielded = decode_item_operation_semantics(pirate, &raw, 4);
        assert_eq!(shielded.source_object_id, Some(0xE003));
        assert_eq!(shielded.target_object_id, Some(0xE003));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive table keeps all reconstructed class/state writer shapes auditable together"
    )]
    fn every_implemented_external_state_maps_after_its_exact_writer_shape() {
        #[derive(Clone, Copy)]
        struct Case {
            pair: (u32, u32),
            state: u32,
            length: usize,
            meaning: ItemLifecycleMeaning,
            phase: Option<u8>,
        }

        const BARRICADE: (u32, u32) = (0x1D86_04A3, 0x2D06_05C2);
        const BANANA: (u32, u32) = (0x1090_0367, 0x1CB3_0486);
        const MINE: (u32, u32) = (0x0A6B_02AF, 0x1450_03CE);
        const ROCKET: (u32, u32) = (0x1129_038E, 0x1D4C_04AD);
        const SUPER_MAG: (u32, u32) = (0x198F_044A, 0x27F0_0569);
        const WATERBOMB: (u32, u32) = (0x1E65_04C9, 0x2DE5_05E8);
        const WATERFLY: (u32, u32) = (0x19AE_0474, 0x280F_0593);
        let cases = [
            Case {
                pair: BARRICADE,
                state: 0,
                length: 25,
                meaning: ItemLifecycleMeaning::Initialize,
                phase: Some(0),
            },
            Case {
                pair: BARRICADE,
                state: 1,
                length: 73,
                meaning: ItemLifecycleMeaning::Place,
                phase: Some(1),
            },
            Case {
                pair: BARRICADE,
                state: 2,
                length: 25,
                meaning: ItemLifecycleMeaning::Impact,
                phase: Some(3),
            },
            Case {
                pair: BARRICADE,
                state: 3,
                length: 25,
                meaning: ItemLifecycleMeaning::Resolve,
                phase: Some(4),
            },
            Case {
                pair: BARRICADE,
                state: 4,
                length: 26,
                meaning: ItemLifecycleMeaning::Remove,
                phase: None,
            },
            Case {
                pair: BANANA,
                state: 1,
                length: 74,
                meaning: ItemLifecycleMeaning::Place,
                phase: Some(0),
            },
            Case {
                pair: BANANA,
                state: 2,
                length: 30,
                meaning: ItemLifecycleMeaning::Impact,
                phase: None,
            },
            Case {
                pair: BANANA,
                state: 3,
                length: 30,
                meaning: ItemLifecycleMeaning::Resolve,
                phase: None,
            },
            Case {
                pair: MINE,
                state: 1,
                length: 77,
                meaning: ItemLifecycleMeaning::Place,
                phase: Some(0),
            },
            Case {
                pair: MINE,
                state: 2,
                length: 29,
                meaning: ItemLifecycleMeaning::Impact,
                phase: None,
            },
            Case {
                pair: MINE,
                state: 3,
                length: 29,
                meaning: ItemLifecycleMeaning::NoClientAction,
                phase: None,
            },
            Case {
                pair: MINE,
                state: 4,
                length: 29,
                meaning: ItemLifecycleMeaning::Resolve,
                phase: Some(4),
            },
            Case {
                pair: MINE,
                state: 5,
                length: 29,
                meaning: ItemLifecycleMeaning::Remove,
                phase: None,
            },
            Case {
                pair: MINE,
                state: 6,
                length: 68,
                meaning: ItemLifecycleMeaning::Respawn,
                phase: Some(0),
            },
            Case {
                pair: ROCKET,
                state: 1,
                length: 82,
                meaning: ItemLifecycleMeaning::Launch,
                phase: Some(1),
            },
            Case {
                pair: ROCKET,
                state: 2,
                length: 73,
                meaning: ItemLifecycleMeaning::Impact,
                phase: Some(2),
            },
            Case {
                pair: ROCKET,
                state: 3,
                length: 20,
                meaning: ItemLifecycleMeaning::Resolve,
                phase: Some(3),
            },
            Case {
                pair: ROCKET,
                state: 4,
                length: 20,
                meaning: ItemLifecycleMeaning::Retarget,
                phase: None,
            },
            Case {
                pair: ROCKET,
                state: 5,
                length: 16,
                meaning: ItemLifecycleMeaning::Remove,
                phase: None,
            },
            Case {
                pair: ROCKET,
                state: 6,
                length: 16,
                meaning: ItemLifecycleMeaning::NoClientAction,
                phase: None,
            },
            Case {
                pair: ROCKET,
                state: 7,
                length: 24,
                meaning: ItemLifecycleMeaning::RebindSource,
                phase: Some(6),
            },
            Case {
                pair: ROCKET,
                state: 8,
                length: 20,
                meaning: ItemLifecycleMeaning::Resolve,
                phase: Some(7),
            },
            Case {
                pair: ROCKET,
                state: 9,
                length: 20,
                meaning: ItemLifecycleMeaning::Resolve,
                phase: Some(8),
            },
            Case {
                pair: ROCKET,
                state: 10,
                length: 20,
                meaning: ItemLifecycleMeaning::Resolve,
                phase: Some(9),
            },
            Case {
                pair: SUPER_MAG,
                state: 1,
                length: 29,
                meaning: ItemLifecycleMeaning::Activate,
                phase: Some(0),
            },
            Case {
                pair: SUPER_MAG,
                state: 2,
                length: 16,
                meaning: ItemLifecycleMeaning::Resolve,
                phase: Some(4),
            },
            Case {
                pair: WATERBOMB,
                state: 1,
                length: 125,
                meaning: ItemLifecycleMeaning::Launch,
                phase: Some(0),
            },
            Case {
                pair: WATERBOMB,
                state: 2,
                length: 29,
                meaning: ItemLifecycleMeaning::Impact,
                phase: Some(2),
            },
            Case {
                pair: WATERBOMB,
                state: 3,
                length: 29,
                meaning: ItemLifecycleMeaning::Impact,
                phase: Some(3),
            },
            Case {
                pair: WATERBOMB,
                state: 4,
                length: 29,
                meaning: ItemLifecycleMeaning::Remove,
                phase: Some(4),
            },
            Case {
                pair: WATERFLY,
                state: 1,
                length: 77,
                meaning: ItemLifecycleMeaning::Launch,
                phase: Some(0),
            },
            Case {
                pair: WATERFLY,
                state: 2,
                length: 65,
                meaning: ItemLifecycleMeaning::Impact,
                phase: None,
            },
            Case {
                pair: WATERFLY,
                state: 3,
                length: 68,
                meaning: ItemLifecycleMeaning::Impact,
                phase: Some(2),
            },
            Case {
                pair: WATERFLY,
                state: 4,
                length: 28,
                meaning: ItemLifecycleMeaning::Resolve,
                phase: Some(3),
            },
            Case {
                pair: WATERFLY,
                state: 5,
                length: 20,
                meaning: ItemLifecycleMeaning::Remove,
                phase: Some(4),
            },
        ];

        for case in cases {
            let schema = schema(case.pair);
            let mut raw = vec![0; case.length];
            put_u32(&mut raw, 8, 0x7000_0001);
            match schema.state_field {
                crate::game_slot_item_schema::ItemOperationStateField::U8 { offset } => {
                    raw[offset] = u8::try_from(case.state).unwrap();
                }
                crate::game_slot_item_schema::ItemOperationStateField::U32 { offset } => {
                    put_u32(&mut raw, offset, case.state);
                }
            }
            let validated = schema.validate(&raw).unwrap();
            let decoded = decode_item_operation_semantics(schema, &raw, validated.state());
            assert_eq!(
                decoded.meaning, case.meaning,
                "{} state {}",
                schema.class_name, case.state
            );
            assert_eq!(
                decoded.native_phase, case.phase,
                "{} state {}",
                schema.class_name, case.state
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one family table keeps every recovered writer state adjacent and auditable"
    )]
    fn second_semantic_pass_maps_bomb_and_mine_families() {
        let ordinary_bomb_states = [
            (1_u32, 120_usize, ItemLifecycleMeaning::Launch, Some(0)),
            (2, 28, ItemLifecycleMeaning::Impact, Some(2)),
            (3, 28, ItemLifecycleMeaning::Resolve, Some(3)),
            (4, 28, ItemLifecycleMeaning::Resolve, Some(4)),
        ];
        let cases = [
            ((0x1900_0448, 0x2761_0567), ordinary_bomb_states),
            ((0x19EB_046D, 0x284C_058C), ordinary_bomb_states),
        ];
        for (pair, states) in cases {
            let schema = schema(pair);
            for (state, length, meaning, phase) in states {
                let mut raw = vec![0; length];
                put_u32(&mut raw, 8, 0x7400_0001);
                put_u32(&mut raw, 12, state);
                let validated = schema.validate(&raw).unwrap();
                let decoded = decode_item_operation_semantics(schema, &raw, validated.state());
                assert_eq!(
                    (decoded.meaning, decoded.native_phase),
                    (meaning, phase),
                    "{} state {state}",
                    schema.class_name
                );
            }
        }

        let cases = [
            (
                (0x2DC1_05C8, 0x409E_06E7),
                vec![
                    (1_u32, 121_usize, ItemLifecycleMeaning::Launch, Some(0)),
                    (2, 33, ItemLifecycleMeaning::Impact, Some(2)),
                    (3, 33, ItemLifecycleMeaning::Resolve, Some(4)),
                ],
            ),
            (
                (0x42E4_071F, 0x591E_083E),
                vec![
                    (1, 132, ItemLifecycleMeaning::Launch, Some(0)),
                    (2, 28, ItemLifecycleMeaning::Impact, Some(2)),
                    (3, 24, ItemLifecycleMeaning::Resolve, Some(3)),
                    (4, 24, ItemLifecycleMeaning::Resolve, Some(4)),
                ],
            ),
            (
                (0x2954_059D, 0x3B12_06BC),
                vec![
                    (1, 132, ItemLifecycleMeaning::Launch, Some(0)),
                    (2, 28, ItemLifecycleMeaning::Impact, Some(2)),
                    (3, 24, ItemLifecycleMeaning::Resolve, Some(3)),
                    (4, 24, ItemLifecycleMeaning::Resolve, Some(4)),
                ],
            ),
            (
                (0x6381_08BF, 0x7E37_09DE),
                vec![
                    (1, 132, ItemLifecycleMeaning::Launch, Some(0)),
                    (2, 32, ItemLifecycleMeaning::Impact, Some(2)),
                    (3, 28, ItemLifecycleMeaning::Resolve, Some(4)),
                ],
            ),
            (
                (0x41CC_070F, 0x3996_067F),
                vec![
                    (1, 24, ItemLifecycleMeaning::Launch, Some(0)),
                    (2, 28, ItemLifecycleMeaning::Impact, Some(2)),
                    (3, 24, ItemLifecycleMeaning::Resolve, Some(3)),
                    (4, 24, ItemLifecycleMeaning::Resolve, Some(4)),
                ],
            ),
            (
                (0x2DDA_05D7, 0x40B7_06F6),
                vec![
                    (1, 24, ItemLifecycleMeaning::Launch, Some(0)),
                    (2, 28, ItemLifecycleMeaning::Impact, Some(2)),
                    (3, 24, ItemLifecycleMeaning::Resolve, Some(3)),
                    (4, 24, ItemLifecycleMeaning::Resolve, Some(4)),
                ],
            ),
            (
                (0x48D7_0757, 0x6030_0876),
                vec![
                    (1, 24, ItemLifecycleMeaning::Launch, Some(0)),
                    (2, 32, ItemLifecycleMeaning::Impact, Some(2)),
                    (3, 28, ItemLifecycleMeaning::Resolve, Some(4)),
                ],
            ),
            (
                (0x2EC5_05FC, 0x41A2_071B),
                vec![
                    (1, 24, ItemLifecycleMeaning::Launch, Some(0)),
                    (2, 28, ItemLifecycleMeaning::Impact, Some(2)),
                    (3, 28, ItemLifecycleMeaning::Resolve, Some(3)),
                    (4, 28, ItemLifecycleMeaning::Resolve, Some(4)),
                ],
            ),
            (
                (0x196A_0455, 0x27CB_0574),
                vec![
                    (1, 24, ItemLifecycleMeaning::Launch, Some(0)),
                    (2, 28, ItemLifecycleMeaning::Impact, Some(2)),
                    (3, 28, ItemLifecycleMeaning::Resolve, Some(3)),
                    (4, 28, ItemLifecycleMeaning::Resolve, Some(4)),
                ],
            ),
            (
                (0x1E04_04B2, 0x2D84_05D1),
                vec![
                    (1, 73, ItemLifecycleMeaning::Place, Some(0)),
                    (2, 29, ItemLifecycleMeaning::Impact, Some(2)),
                    (3, 29, ItemLifecycleMeaning::Resolve, Some(3)),
                    (4, 29, ItemLifecycleMeaning::Resolve, Some(4)),
                    (7, 29, ItemLifecycleMeaning::NoClientAction, None),
                ],
            ),
            (
                (0x1909_043E, 0x276A_055D),
                vec![
                    (1, 85, ItemLifecycleMeaning::Place, Some(0)),
                    (2, 33, ItemLifecycleMeaning::Impact, None),
                    (3, 33, ItemLifecycleMeaning::Resolve, None),
                    (4, 33, ItemLifecycleMeaning::NoClientAction, None),
                    (5, 24, ItemLifecycleMeaning::Resolve, None),
                ],
            ),
        ];
        for (pair, states) in cases {
            let schema = schema(pair);
            for (state, length, meaning, phase) in states {
                let mut raw = vec![0; length];
                put_u32(&mut raw, 8, 0x7400_0002);
                match schema.state_field {
                    crate::game_slot_item_schema::ItemOperationStateField::U8 { offset } => {
                        raw[offset] = u8::try_from(state).unwrap();
                    }
                    crate::game_slot_item_schema::ItemOperationStateField::U32 { offset } => {
                        put_u32(&mut raw, offset, state);
                    }
                }
                if schema.class_name == "GopTimeMine" && matches!(state, 2 | 3) {
                    raw[28] = 1;
                }
                let validated = schema.validate(&raw).unwrap();
                let decoded = decode_item_operation_semantics(schema, &raw, validated.state());
                assert_eq!(
                    (decoded.meaning, decoded.native_phase),
                    (meaning, phase),
                    "{} state {state}",
                    schema.class_name
                );
            }
        }
    }

    #[test]
    fn time_mine_target_branch_is_flagged_and_runtime_conditional() {
        let schema = schema((0x1909_043E, 0x276A_055D));
        let mut raw = vec![0; 33];
        put_u32(&mut raw, 8, 0x7400_0010);
        put_u32(&mut raw, 16, 2);
        put_u32(&mut raw, 20, 0x7500_0010);
        put_u32(&mut raw, 24, 0x7600_0010);
        put_u32(&mut raw, 29, 0x7700_0010);

        let no_target = decode_item_operation_semantics(schema, &raw, 2);
        assert_eq!(no_target.meaning, ItemLifecycleMeaning::Resolve);
        assert_eq!(no_target.native_phase, None);
        assert_eq!(no_target.source_object_id, None);
        assert_eq!(no_target.target_object_id, None);

        raw[28] = 1;
        let conditional_impact = decode_item_operation_semantics(schema, &raw, 2);
        assert_eq!(conditional_impact.meaning, ItemLifecycleMeaning::Impact);
        assert_eq!(conditional_impact.native_phase, None);
        assert_eq!(conditional_impact.source_object_id, None);
        assert_eq!(conditional_impact.target_object_id, Some(0x7600_0010));
    }

    #[test]
    fn recovered_runtime_consumers_keep_their_exact_actor_bindings() {
        const TOKEN: u32 = 0x7100_0001;
        const TARGET: u32 = 0x7200_0002;
        const SOURCE: u32 = 0x7300_0003;

        let big = schema((0x276B_0567, 0x3929_0686));
        let mut raw = vec![0; 29];
        put_u32(&mut raw, 8, 0x7000_0000);
        raw[12] = 0xA5;
        put_u32(&mut raw, 13, 4);
        put_u32(&mut raw, 17, TOKEN);
        put_u32(&mut raw, 21, TARGET);
        put_u32(&mut raw, 25, SOURCE);
        let validated = big.validate(&raw).unwrap();
        assert_eq!(validated.state(), 4);
        let decoded = decode_item_operation_semantics(big, &raw, validated.state());
        assert_eq!(decoded.meaning, ItemLifecycleMeaning::Resolve);
        assert_eq!(decoded.native_phase, Some(4));
        assert_eq!(decoded.transition_token, Some(TOKEN));
        assert_eq!(decoded.target_object_id, Some(TARGET));
        assert_eq!(decoded.source_object_id, Some(SOURCE));
        assert_eq!(decoded.variant, Some(0xA5));

        for (phase, meaning) in [
            (0, ItemLifecycleMeaning::Activate),
            (2, ItemLifecycleMeaning::Impact),
            (3, ItemLifecycleMeaning::Impact),
            (4, ItemLifecycleMeaning::Resolve),
        ] {
            put_u32(&mut raw, 13, phase);
            let decoded = decode_item_operation_semantics(big, &raw, phase);
            assert_eq!(decoded.meaning, meaning);
            assert_eq!(decoded.native_phase, u8::try_from(phase).ok());
        }

        for pair in [
            (0x41CC_070F, 0x3996_067F),
            (0x2DDA_05D7, 0x40B7_06F6),
            (0x2954_059D, 0x3B12_06BC),
        ] {
            let schema = schema(pair);
            let mut raw = vec![0; 28];
            put_u32(&mut raw, 12, 2);
            put_u32(&mut raw, 16, TOKEN);
            put_u32(&mut raw, 20, TARGET);
            put_u32(&mut raw, 24, SOURCE);
            let decoded = decode_item_operation_semantics(schema, &raw, 2);
            assert_eq!(decoded.meaning, ItemLifecycleMeaning::Impact);
            assert_eq!(decoded.target_object_id, Some(TARGET));
            assert_eq!(decoded.source_object_id, Some(SOURCE));
        }

        let infected = schema((0x48D7_0757, 0x6030_0876));
        let mut raw = vec![0; 32];
        put_u32(&mut raw, 12, 2);
        put_u32(&mut raw, 16, TOKEN);
        put_u32(&mut raw, 20, TARGET);
        put_u32(&mut raw, 24, 0x1122_3344);
        put_u32(&mut raw, 28, SOURCE);
        let decoded = decode_item_operation_semantics(infected, &raw, 2);
        assert_eq!(decoded.target_object_id, Some(TARGET));
        assert_eq!(decoded.source_object_id, Some(SOURCE));

        let snow = schema((0x2EC5_05FC, 0x41A2_071B));
        let mut raw = vec![0; 28];
        put_u32(&mut raw, 12, 3);
        put_u32(&mut raw, 16, TOKEN);
        put_u32(&mut raw, 20, TARGET);
        put_u32(&mut raw, 24, SOURCE);
        let decoded = decode_item_operation_semantics(snow, &raw, 3);
        assert_eq!(decoded.target_object_id, Some(TARGET));
        assert_eq!(decoded.source_object_id, Some(TARGET));
    }

    #[test]
    fn each_rocket_variant_keeps_its_own_consumer_followup_limit() {
        let variants = [
            ((0x2261_0510, 0x3300_062F), 10_u32),
            ((0x3A06_069F, 0x4F21_07BE), 9),
            ((0x228A_0514, 0x3329_0633), 10),
            ((0x1584_0409, 0x22C6_0528), 7),
            ((0x2882_0589, 0x3A40_06A8), 9),
        ];
        for (pair, last_transition_state) in variants {
            let schema = schema(pair);
            for state in 1..=10 {
                let length = match state {
                    1 => 77,
                    2 => 72,
                    3 if schema.class_name == "GopGoldRocket" => 22,
                    3 | 4 | 8..=10 => 20,
                    5 | 6 => 16,
                    7 => 24,
                    _ => unreachable!(),
                };
                let mut raw = vec![0; length];
                put_u32(&mut raw, 8, 0x7000_0002);
                put_u32(&mut raw, 12, state);
                let validated = schema.validate(&raw).unwrap();
                let decoded = decode_item_operation_semantics(schema, &raw, validated.state());
                let expected = match state {
                    1 => (ItemLifecycleMeaning::Launch, Some(1)),
                    2 => (ItemLifecycleMeaning::Impact, Some(2)),
                    3 => (ItemLifecycleMeaning::Resolve, Some(3)),
                    4 => (ItemLifecycleMeaning::Retarget, None),
                    5 => (ItemLifecycleMeaning::Remove, None),
                    6 => (ItemLifecycleMeaning::NoClientAction, None),
                    7 => (ItemLifecycleMeaning::RebindSource, Some(6)),
                    value if value <= last_transition_state => {
                        (ItemLifecycleMeaning::Resolve, u8::try_from(value - 1).ok())
                    }
                    _ => (ItemLifecycleMeaning::NoClientAction, None),
                };
                assert_eq!(
                    (decoded.meaning, decoded.native_phase),
                    expected,
                    "{} state {state}",
                    schema.class_name
                );
            }
        }
    }

    #[test]
    fn waterfly_variants_keep_their_individual_terminal_contracts() {
        let shared_variants = [(0x49AB_0796, 0x6104_08B5), (0x2F69_061B, 0x4246_073A)];
        let shared_states = [
            (1_u32, 77_usize, ItemLifecycleMeaning::Launch, Some(0)),
            (2, 65, ItemLifecycleMeaning::Impact, None),
            (3, 68, ItemLifecycleMeaning::Impact, Some(2)),
            (4, 28, ItemLifecycleMeaning::Resolve, Some(3)),
            (5, 20, ItemLifecycleMeaning::Remove, Some(4)),
        ];

        for pair in shared_variants {
            let schema = schema(pair);
            for (state, length, meaning, phase) in shared_states {
                let mut raw = vec![0; length];
                put_u32(&mut raw, 8, 0x7000_0003);
                put_u32(&mut raw, 12, state);
                let validated = schema.validate(&raw).unwrap();
                let decoded = decode_item_operation_semantics(schema, &raw, validated.state());
                assert_eq!(
                    (decoded.meaning, decoded.native_phase),
                    (meaning, phase),
                    "{} state {state}",
                    schema.class_name
                );
            }
        }

        let schema = schema((0x2EE3_05F4, 0x41C0_0713));
        let waterbomb_fly_states = [
            (1_u32, 77_usize, ItemLifecycleMeaning::Launch, Some(0)),
            (2, 64, ItemLifecycleMeaning::Impact, None),
            (3, 28, ItemLifecycleMeaning::Resolve, Some(3)),
            (4, 28, ItemLifecycleMeaning::Resolve, Some(4)),
            (5, 20, ItemLifecycleMeaning::NoClientAction, None),
            (6, 16, ItemLifecycleMeaning::Remove, None),
        ];
        for (state, length, meaning, phase) in waterbomb_fly_states {
            let mut raw = vec![0; length];
            put_u32(&mut raw, 8, 0x7000_0004);
            put_u32(&mut raw, 12, state);
            let validated = schema.validate(&raw).unwrap();
            let decoded = decode_item_operation_semantics(schema, &raw, validated.state());
            assert_eq!(
                (decoded.meaning, decoded.native_phase),
                (meaning, phase),
                "{} state {state}",
                schema.class_name
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the literal table keeps each recovered and anomalous branch auditable"
    )]
    fn fourth_pass_keeps_exact_shapes_separate_from_proven_meanings() {
        const TOKEN: u32 = 0x7100_0001;
        const SOURCE: u32 = 0x7200_0002;
        const TARGET: u32 = 0x7300_0003;

        struct Case {
            pair: (u32, u32),
            state: u32,
            length: usize,
            token_offset: Option<usize>,
            source_offset: Option<usize>,
            target_offset: Option<usize>,
            variant_offset: Option<usize>,
            meaning: ItemLifecycleMeaning,
            phase: Option<u8>,
        }

        let cases = [
            Case {
                pair: (0x0D49_030D, 0x184D_042C),
                state: 0,
                length: 25,
                token_offset: Some(16),
                source_offset: Some(21),
                target_offset: None,
                variant_offset: Some(20),
                meaning: ItemLifecycleMeaning::Activate,
                phase: Some(0),
            },
            Case {
                pair: (0x0D49_030D, 0x184D_042C),
                state: 2,
                length: 28,
                token_offset: Some(16),
                source_offset: Some(20),
                target_offset: Some(24),
                variant_offset: None,
                meaning: ItemLifecycleMeaning::Impact,
                phase: Some(2),
            },
            Case {
                pair: (0x07AE_0248, 0x1074_0367),
                state: 0,
                length: 26,
                token_offset: Some(16),
                source_offset: Some(22),
                target_offset: None,
                variant_offset: Some(20),
                meaning: ItemLifecycleMeaning::Activate,
                phase: Some(0),
            },
            Case {
                pair: (0x0D8B_032B, 0x188F_044A),
                state: 1,
                length: 29,
                token_offset: Some(16),
                source_offset: Some(20),
                target_offset: Some(24),
                variant_offset: Some(28),
                meaning: ItemLifecycleMeaning::Activate,
                phase: Some(0),
            },
            Case {
                pair: (0x10C3_0382, 0x1CE6_04A1),
                state: 1,
                length: 78,
                token_offset: Some(16),
                source_offset: Some(20),
                target_offset: Some(24),
                variant_offset: Some(77),
                meaning: ItemLifecycleMeaning::Launch,
                phase: Some(0),
            },
            Case {
                pair: (0x1942_0457, 0x27A3_0576),
                state: 1,
                length: 30,
                token_offset: Some(20),
                source_offset: Some(16),
                target_offset: Some(24),
                variant_offset: Some(28),
                meaning: ItemLifecycleMeaning::Activate,
                phase: Some(0),
            },
            Case {
                pair: (0x196B_0451, 0x27CC_0570),
                state: 1,
                length: 29,
                token_offset: Some(24),
                source_offset: Some(16),
                target_offset: None,
                variant_offset: Some(28),
                meaning: ItemLifecycleMeaning::Activate,
                phase: Some(0),
            },
            Case {
                pair: (0x196B_0451, 0x27CC_0570),
                state: 2,
                length: 29,
                token_offset: Some(24),
                source_offset: Some(16),
                target_offset: Some(20),
                variant_offset: Some(28),
                meaning: ItemLifecycleMeaning::Impact,
                phase: Some(1),
            },
            Case {
                pair: (0x2E54_05E8, 0x4131_0707),
                state: 0,
                length: 26,
                token_offset: Some(16),
                source_offset: Some(21),
                target_offset: None,
                variant_offset: Some(20),
                meaning: ItemLifecycleMeaning::Activate,
                phase: None,
            },
            Case {
                pair: (0x2262_0502, 0x3301_0621),
                state: 0,
                length: 30,
                token_offset: Some(16),
                source_offset: Some(24),
                target_offset: Some(20),
                variant_offset: Some(29),
                meaning: ItemLifecycleMeaning::Launch,
                phase: Some(0),
            },
            Case {
                pair: (0x2262_0502, 0x3301_0621),
                state: 4,
                length: 29,
                token_offset: Some(16),
                source_offset: None,
                target_offset: Some(20),
                variant_offset: Some(28),
                meaning: ItemLifecycleMeaning::Resolve,
                phase: Some(6),
            },
            Case {
                pair: (0x3C6F_06D4, 0x518A_07F3),
                state: 1,
                length: 58,
                token_offset: Some(16),
                source_offset: Some(20),
                target_offset: None,
                variant_offset: Some(56),
                meaning: ItemLifecycleMeaning::Launch,
                phase: Some(1),
            },
            Case {
                pair: (0x3C6F_06D4, 0x518A_07F3),
                state: 2,
                length: 24,
                token_offset: None,
                source_offset: None,
                target_offset: None,
                variant_offset: None,
                meaning: ItemLifecycleMeaning::NoClientAction,
                phase: None,
            },
            Case {
                pair: (0x3C6F_06D4, 0x518A_07F3),
                state: 3,
                length: 24,
                token_offset: None,
                source_offset: None,
                target_offset: None,
                variant_offset: None,
                meaning: ItemLifecycleMeaning::NoClientAction,
                phase: None,
            },
        ];

        for case in cases {
            let schema = schema(case.pair);
            let mut raw = vec![0_u8; case.length];
            put_u32(&mut raw, 8, 0x7000_0001);
            put_u32(&mut raw, 12, case.state);
            if let Some(offset) = case.token_offset {
                put_u32(&mut raw, offset, TOKEN);
            }
            if let Some(offset) = case.source_offset {
                put_u32(&mut raw, offset, SOURCE);
            }
            if let Some(offset) = case.target_offset {
                put_u32(&mut raw, offset, TARGET);
            }
            if let Some(offset) = case.variant_offset {
                raw[offset] = 0x5A;
            }

            let validated = schema.validate(&raw).unwrap();
            let decoded = decode_item_operation_semantics(schema, &raw, validated.state());
            assert_eq!(decoded.meaning, case.meaning, "{}", schema.class_name);
            assert_eq!(decoded.native_phase, case.phase, "{}", schema.class_name);
            assert_eq!(
                decoded.transition_token,
                case.token_offset.map(|_| TOKEN),
                "{}",
                schema.class_name
            );
            assert_eq!(
                decoded.source_object_id,
                case.source_offset.map(|_| SOURCE),
                "{}",
                schema.class_name
            );
            assert_eq!(
                decoded.target_object_id,
                case.target_offset.map(|_| TARGET),
                "{}",
                schema.class_name
            );
            assert_eq!(
                decoded.variant,
                case.variant_offset.map(|_| 0x5A),
                "{}",
                schema.class_name
            );
        }

        let spacecraft = schema((0x2262_0502, 0x3301_0621));
        let mut consumer_only = vec![0_u8; 29];
        put_u32(&mut consumer_only, 12, 3);
        assert_eq!(
            decode_item_operation_semantics(spacecraft, &consumer_only, 3).evidence,
            ItemSemanticEvidence::StaticConsumer
        );
        let mut produced_runtime_flag = vec![0_u8; 17];
        put_u32(&mut produced_runtime_flag, 12, 7);
        produced_runtime_flag[16] = 1;
        assert_eq!(
            decode_item_operation_semantics(spacecraft, &produced_runtime_flag, 7).evidence,
            ItemSemanticEvidence::ProducerAndConsumer
        );
    }
}
