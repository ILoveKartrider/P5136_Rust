//! Executable client-side FSM for reconstructed type-12 item consumers.
//!
//! Decoding and transition effects remain independent of the production
//! server.  An inbound consumer never implies an acknowledgement: a deferred
//! outcome is emitted only where the recovered native producer can later
//! originate another operation after a timer, collision, or local guard.

use std::collections::{HashMap, VecDeque};

use crate::{
    DecodeError,
    item_operation::{ConsumedOperation, Meaning, consume},
};

/// The original independently recovered consumer-fixture corpus.
pub const AUDITED_CONSUMER_BRANCH_COUNT: usize = 149;

/// Boss/course-controller branches recovered after the original corpus.
pub const SUPPLEMENTAL_CONSUMER_BRANCH_COUNT: usize = 17;

/// Every currently reconstructed item-consumer branch accepted by this FSM.
pub const TOTAL_CONSUMER_BRANCH_COUNT: usize =
    AUDITED_CONSUMER_BRANCH_COUNT + SUPPLEMENTAL_CONSUMER_BRANCH_COUNT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemClientTransitionOutcome {
    /// The consumer mutates or releases local runtime state without a proven
    /// later network transition.
    LocalOnly,
    /// The consumer arms runtime state whose native producer may emit a later
    /// operation after a timer, collision, or another local condition.
    DeferredOutbound,
    /// The consumer sends another operation before returning.
    ImmediateOutbound,
    /// The codec branch is exact, but its runtime side effect is not.
    UnknownSideEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ItemClientObjectKey {
    pub class_name: &'static str,
    pub object_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemClientObjectState {
    pub state: u32,
    pub meaning: Meaning,
    pub native_phase: Option<u8>,
    pub transition_token: Option<u32>,
    pub source_object_id: Option<u32>,
    pub target_object_id: Option<u32>,
    pub target_object_ids: Vec<u32>,
    pub variant: Option<u8>,
    pub effect_item_id: Option<u16>,
}

impl ItemClientObjectState {
    fn from_operation(operation: &ConsumedOperation) -> Self {
        Self {
            state: operation.state,
            meaning: operation.meaning,
            native_phase: operation.native_phase,
            transition_token: operation.transition_token,
            source_object_id: operation.source_object_id,
            target_object_id: operation.target_object_id,
            target_object_ids: operation.target_object_ids.clone(),
            variant: operation.variant,
            effect_item_id: operation.effect_item_id,
        }
    }
}

/// A scheduler marker, not a fabricated packet.  A mock client must wait for
/// the corresponding native timer/collision guard before producing bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeferredOutboundTransition {
    pub key: ItemClientObjectKey,
    pub consumed_state: u32,
    pub consumed_meaning: Meaning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemClientTransition {
    pub operation: ConsumedOperation,
    pub outcome: ItemClientTransitionOutcome,
    pub previous: Option<ItemClientObjectState>,
    pub current: Option<ItemClientObjectState>,
}

#[derive(Debug, Default)]
pub struct ItemClientFsm {
    objects: HashMap<ItemClientObjectKey, ItemClientObjectState>,
    deferred_outbound: VecDeque<DeferredOutboundTransition>,
    accepted_transition_count: usize,
}

impl ItemClientFsm {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes and applies one complete native item-operation body.
    ///
    /// A decoding failure is transactional: no object, queue, or counter is
    /// changed.  Unknown and explicit no-action branches are observed but do
    /// not fabricate local object state.
    pub fn accept(&mut self, raw: &[u8]) -> Result<ItemClientTransition, DecodeError> {
        let operation = consume(raw)?;
        let outcome = transition_outcome(&operation);
        let key = ItemClientObjectKey {
            class_name: operation.class_name,
            object_id: operation.object_id,
        };
        let previous = self.objects.get(&key).cloned();

        // A newer known lifecycle transition supersedes an older armed
        // producer path. Unknown and explicit no-action branches cannot prove
        // that cancellation and therefore leave the marker untouched.
        if !matches!(
            operation.meaning,
            Meaning::Unknown | Meaning::NoClientAction
        ) {
            self.deferred_outbound
                .retain(|deferred| deferred.key != key);
        }

        match operation.meaning {
            Meaning::Unknown | Meaning::NoClientAction => {}
            Meaning::Remove => {
                self.objects.remove(&key);
            }
            Meaning::Place
            | Meaning::Launch
            | Meaning::Activate
            | Meaning::Impact
            | Meaning::Resolve
            | Meaning::Retarget
            | Meaning::UpdateRuntimeFlag => {
                self.objects
                    .insert(key, ItemClientObjectState::from_operation(&operation));
            }
        }

        if outcome == ItemClientTransitionOutcome::DeferredOutbound {
            self.deferred_outbound
                .push_back(DeferredOutboundTransition {
                    key,
                    consumed_state: operation.state,
                    consumed_meaning: operation.meaning,
                });
        }
        self.accepted_transition_count += 1;
        let current = self.objects.get(&key).cloned();

        Ok(ItemClientTransition {
            operation,
            outcome,
            previous,
            current,
        })
    }

    #[must_use]
    pub fn object(&self, class_name: &str, object_id: u32) -> Option<&ItemClientObjectState> {
        self.objects.iter().find_map(|(key, state)| {
            (key.class_name == class_name && key.object_id == object_id).then_some(state)
        })
    }

    #[must_use]
    pub fn active_object_count(&self) -> usize {
        self.objects.len()
    }

    #[must_use]
    pub const fn accepted_transition_count(&self) -> usize {
        self.accepted_transition_count
    }

    #[must_use]
    pub fn pending_deferred_outbound(&self) -> usize {
        self.deferred_outbound.len()
    }

    #[must_use]
    pub fn take_deferred_outbound(&mut self) -> Vec<DeferredOutboundTransition> {
        self.deferred_outbound.drain(..).collect()
    }

    pub fn reset_race(&mut self) {
        self.objects.clear();
        self.deferred_outbound.clear();
        self.accepted_transition_count = 0;
    }
}

/// Classifies the observable result of one already decoded consumer branch.
///
/// No recovered branch calls the type-12 network writer synchronously.  The
/// `ImmediateOutbound` variant is retained as an explicit future finding,
/// rather than collapsing that distinction into `DeferredOutbound`.
#[must_use]
pub fn transition_outcome(operation: &ConsumedOperation) -> ItemClientTransitionOutcome {
    if operation.meaning == Meaning::Unknown {
        return ItemClientTransitionOutcome::UnknownSideEffect;
    }
    if has_proven_deferred_follow_up(operation) {
        ItemClientTransitionOutcome::DeferredOutbound
    } else {
        ItemClientTransitionOutcome::LocalOnly
    }
}

#[allow(
    clippy::match_same_arms,
    clippy::unnested_or_patterns,
    reason = "class/state rows stay explicit so producer evidence remains auditable"
)]
fn has_proven_deferred_follow_up(operation: &ConsumedOperation) -> bool {
    let class = operation.class_name;
    let state = operation.state;
    match (class, state) {
        // Runtime or world object installation followed by a recovered compact
        // hit/result transition.
        ("GopAngel", 0)
        | ("GopGoldShield", 0)
        | ("GopBalloon" | "GopHeadBand", 1)
        | ("GopBlock" | "GopBoundWall", 1)
        | ("GopDynamite" | "GopHammer", 1)
        | ("GopPress" | "GopRobotBeam" | "GopTombStone", 1)
        | ("GopCubeForBoss", 0)
        | ("GopCloud" | "GopCloud2", 1)
        | ("GopBigTimebomb", 0)
        | ("GopSlotLock", 1)
        | ("GopSpaceCraft", 0 | 2)
        | ("GopStraightRocket", 1)
        | ("GopAreaUfo" | "GopMovingUfo" | "GopUfo", 1)
        | ("GopShield", 1)
        | ("GopSpecialShield", 0 | 2)
        | ("GopThunderbolt", 1)
        | ("GopForceZone", 1)
        | ("GopOil", 1)
        | ("GopSiren", 1)
        | ("GopSpecialSmall", 0)
        // Bomb and mine producers have concrete later timer/collision states.
        | ("GopCokebomb" | "GopSnowbomb", 1..=3)
        | ("GopInfectedBomb", 1..=2)
        | ("GopRollingCokebomb" | "GopRollingbomb", 1..=3)
        | ("GopRollingInfectedbomb", 1..=2)
        | ("GopWaterMine", 1..=3)
        | ("GopItemTimeFlybomb" | "GopTimeCokebomb", 1..=3)
        | ("GopTimeInfectedBomb", 1..=2)
        | ("GopTimeSnowbomb" | "GopTimebomb", 1..=3)
        // LockdownRocket continues only from these recovered active phases;
        // teardown and terminal resolution remain local.
        | ("GopLockdownRocket", 1 | 2 | 4 | 7 | 8)
        // Supplemental controller objects use the same deferred boundary.
        | ("GopBossPrison", 1..=3)
        | ("GopBoundRoad" | "GopFalling", 1..=2)
        | ("GopPiratebomb", 1) => true,
        // TimeMine state 2 arms a later phase only on the target-present
        // impact branch. The zero-flag branch resolves locally.
        ("GopTimeMine", 1 | 3) => true,
        ("GopTimeMine", 2) => operation.meaning == Meaning::Impact,
        _ => false,
    }
}
