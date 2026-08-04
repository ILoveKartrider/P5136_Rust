//! Race-incarnation-bound item object lifecycle authority.
//!
//! The stock client remains the producer of the native `Gop*` payload, but an
//! accepted payload is not published until this registry has admitted its
//! object identity and lifecycle transition. Mutations are planned first and
//! committed only after every outbound queue slot has been reserved by the
//! World actor.

use std::collections::HashMap;

use p5136_core::{
    game_slot_item_semantics::{ItemLifecycleMeaning, ItemSemanticEvidence},
    game_slot_protocol::{
        BarricadeOperation, GameSlotAction, GameSlotBody, GameSlotDropReason,
        GameSlotRelayAudience, ItemOperation, parse_game_slot_packet,
    },
};
use thiserror::Error;

pub(crate) const MAX_RACE_OBJECTS: usize = 1_024;

/// Result of admitting one independently decoded type-12 operation through
/// the same fresh race-object registry path used by the World actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemOperationAuditDisposition {
    PublishTracked,
    PublishUntracked,
    SuppressDuplicate,
}

/// Evidence returned by [`audit_game_slot_item_operation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemOperationServerAudit {
    pub class_name: &'static str,
    pub state: u32,
    pub disposition: ItemOperationAuditDisposition,
    /// Exact owned bytes that the normal relay path would publish.
    pub relay_bytes: Vec<u8>,
}

/// Failure of the executable server-side item-operation audit gate.
#[derive(Debug, Error)]
pub enum ItemOperationServerAuditError {
    #[error(transparent)]
    Decode(#[from] GameSlotDropReason),
    #[error("decoded GameSlot packet is not a strict item operation")]
    StrictItemOperationRequired,
    #[error(
        "GameSlot packet claims player {claimed_player_id}, but the audit reporter is player {reporter_player_id}"
    )]
    ClaimedPlayerMismatch {
        claimed_player_id: u8,
        reporter_player_id: u8,
    },
    #[error(
        "GameSlot item-operation mask is 0x{actual:08X}; the synthetic frozen roster requires 0x{expected:08X}"
    )]
    RecipientMaskMismatch { actual: u32, expected: u32 },
    #[error("decoded item operation is not an all-race-peers byte-preserving relay")]
    UnexpectedRelayAction,
    #[error(
        "isolated audit cannot bind explicit owner player {owner_player_id} to reporter player {reporter_player_id}"
    )]
    ExplicitOwnerMismatch {
        owner_player_id: u8,
        reporter_player_id: u8,
    },
    #[error("race-object registry rejected the decoded operation: {detail}")]
    RegistryAdmission { detail: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct RaceObjectActor {
    pub(crate) user_no: u32,
    pub(crate) generation: u64,
    pub(crate) player_id: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct RaceObjectClass {
    pub(crate) operation_hash: u32,
    pub(crate) base_hash: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RaceObjectPhase {
    Initialized,
    Active,
    Hit,
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RaceObjectLifecycleEvent {
    Installed,
    Activated,
    Hit,
    Removed,
    StateUpdated(u32),
    RuntimeFlagUpdated(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RaceObjectDuplicateKind {
    Install,
    Activation,
    Hit,
    Removal,
    TerminalState,
    RuntimeFlagUpdate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RaceObjectOperation {
    pub(crate) race_epoch: u64,
    pub(crate) class: RaceObjectClass,
    pub(crate) class_name: &'static str,
    pub(crate) object_id: u32,
    pub(crate) state: u32,
    pub(crate) meaning: ItemLifecycleMeaning,
    pub(crate) evidence: ItemSemanticEvidence,
    pub(crate) transition_token: Option<u32>,
    pub(crate) source_object_id: Option<u32>,
    pub(crate) target_object_id: Option<u32>,
    pub(crate) variant: Option<u8>,
    pub(crate) reporter: RaceObjectActor,
    /// A native payload may identify the installer independently from the
    /// reporter. Barricade hits are the important retained example: the victim
    /// reports state 2 while the nested owner remains the original installer.
    pub(crate) owner_claim: Option<RaceObjectActor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RaceObjectKey {
    race_epoch: u64,
    object_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RaceObjectRecord {
    class: RaceObjectClass,
    class_name: &'static str,
    owner: RaceObjectActor,
    last_reporter: RaceObjectActor,
    phase: RaceObjectPhase,
    last_state: u32,
    last_meaning: ItemLifecycleMeaning,
    last_transition_token: Option<u32>,
    last_source_object_id: Option<u32>,
    last_target_object_id: Option<u32>,
    runtime_flag: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RaceObjectMutation {
    expected_revision: u64,
    key: RaceObjectKey,
    next: RaceObjectRecord,
    events: [Option<RaceObjectLifecycleEvent>; 2],
}

impl RaceObjectMutation {
    pub(crate) const fn object_id(&self) -> u32 {
        self.key.object_id
    }

    pub(crate) const fn phase(&self) -> RaceObjectPhase {
        self.next.phase
    }

    pub(crate) const fn owner(&self) -> RaceObjectActor {
        self.next.owner
    }

    pub(crate) const fn reporter(&self) -> RaceObjectActor {
        self.next.last_reporter
    }

    pub(crate) const fn class_name(&self) -> &'static str {
        self.next.class_name
    }

    pub(crate) const fn state(&self) -> u32 {
        self.next.last_state
    }

    pub(crate) const fn meaning(&self) -> ItemLifecycleMeaning {
        self.next.last_meaning
    }

    pub(crate) const fn transition_token(&self) -> Option<u32> {
        self.next.last_transition_token
    }

    pub(crate) const fn source_object_id(&self) -> Option<u32> {
        self.next.last_source_object_id
    }

    pub(crate) const fn target_object_id(&self) -> Option<u32> {
        self.next.last_target_object_id
    }

    #[cfg(test)]
    pub(crate) const fn runtime_flag(&self) -> Option<u8> {
        self.next.runtime_flag
    }

    pub(crate) fn events(&self) -> impl Iterator<Item = RaceObjectLifecycleEvent> + '_ {
        self.events.iter().flatten().copied()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RaceObjectAdmission {
    PublishTracked(RaceObjectMutation),
    PublishUntracked,
    SuppressDuplicate {
        object_id: u32,
        class_name: &'static str,
        phase: RaceObjectPhase,
        state: u32,
        kind: RaceObjectDuplicateKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum RaceObjectRegistryError {
    #[error(
        "race {race_epoch} object registry reached its {maximum}-object bound while admitting 0x{object_id:08X}"
    )]
    Capacity {
        race_epoch: u64,
        object_id: u32,
        maximum: usize,
    },
    #[error(
        "race {race_epoch} object 0x{object_id:08X} changed class from 0x{expected_operation_hash:08X}/0x{expected_base_hash:08X} to 0x{actual_operation_hash:08X}/0x{actual_base_hash:08X}"
    )]
    ClassMismatch {
        race_epoch: u64,
        object_id: u32,
        expected_operation_hash: u32,
        expected_base_hash: u32,
        actual_operation_hash: u32,
        actual_base_hash: u32,
    },
    #[error(
        "race {race_epoch} object 0x{object_id:08X} owner claim player {actual_player_id} generation {actual_generation} does not match player {expected_player_id} generation {expected_generation}"
    )]
    OwnerMismatch {
        race_epoch: u64,
        object_id: u32,
        expected_player_id: u8,
        expected_generation: u64,
        actual_player_id: u8,
        actual_generation: u64,
    },
    #[error(
        "race object registry plan revision {planned_revision} is stale; current revision is {current_revision}"
    )]
    StalePlan {
        planned_revision: u64,
        current_revision: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RaceObjectRegistry {
    revision: u64,
    objects: HashMap<RaceObjectKey, RaceObjectRecord>,
}

pub(crate) fn plan_item_operation(
    registry: &RaceObjectRegistry,
    race_epoch: u64,
    operation: &ItemOperation,
    reporter: RaceObjectActor,
    owner_claim: Option<RaceObjectActor>,
) -> Result<RaceObjectAdmission, RaceObjectRegistryError> {
    registry.plan(RaceObjectOperation {
        race_epoch,
        class: RaceObjectClass {
            operation_hash: operation.operation_hash,
            base_hash: operation.operation_base_hash,
        },
        class_name: operation.schema.class_name,
        object_id: operation.object_id,
        state: operation.state,
        meaning: operation.semantics.meaning,
        evidence: operation.semantics.evidence,
        transition_token: operation.semantics.transition_token,
        source_object_id: operation.semantics.source_object_id,
        target_object_id: operation.semantics.target_object_id,
        variant: operation.semantics.variant,
        reporter,
        owner_claim,
    })
}

/// Runs one complete type-12 wire packet through the production decoder and
/// the World actor's race-object admission mapping using an isolated registry.
///
/// This is an executable reverse-engineering audit boundary, not an alternate
/// gameplay implementation. The caller supplies the reporter and peer mask a
/// frozen two-client roster would require; accepted relay bytes come directly
/// from [`p5136_core::game_slot_protocol::ParsedGameSlotPacket::into_raw`].
pub fn audit_game_slot_item_operation(
    packet: &[u8],
    reporter_player_id: u8,
    expected_peer_mask: u32,
) -> Result<ItemOperationServerAudit, ItemOperationServerAuditError> {
    let parsed = parse_game_slot_packet(packet)?;
    if parsed.player_id() != reporter_player_id {
        return Err(ItemOperationServerAuditError::ClaimedPlayerMismatch {
            claimed_player_id: parsed.player_id(),
            reporter_player_id,
        });
    }
    if parsed.item_or_recipient_mask() != expected_peer_mask {
        return Err(ItemOperationServerAuditError::RecipientMaskMismatch {
            actual: parsed.item_or_recipient_mask(),
            expected: expected_peer_mask,
        });
    }
    if parsed.action()
        != GameSlotAction::RelayOriginal(GameSlotRelayAudience::AllRacePeersMaskMatch)
    {
        return Err(ItemOperationServerAuditError::UnexpectedRelayAction);
    }
    let operation = match parsed.body() {
        GameSlotBody::ItemOperation(operation) => *operation,
        _ => return Err(ItemOperationServerAuditError::StrictItemOperationRequired),
    };

    let reporter = RaceObjectActor {
        user_no: 1,
        generation: 1,
        player_id: reporter_player_id,
    };
    let explicit_owner_id = match operation.barricade {
        Some(BarricadeOperation::Placement(placement)) => Some(placement.owner_id),
        Some(BarricadeOperation::Transition(transition)) => Some(transition.owner_id),
        None => None,
    };
    let owner_claim = if let Some(owner_player_id) = explicit_owner_id {
        if owner_player_id != reporter_player_id {
            return Err(ItemOperationServerAuditError::ExplicitOwnerMismatch {
                owner_player_id,
                reporter_player_id,
            });
        }
        Some(reporter)
    } else if matches!(
        operation.semantics.meaning,
        ItemLifecycleMeaning::Initialize
            | ItemLifecycleMeaning::Place
            | ItemLifecycleMeaning::Launch
            | ItemLifecycleMeaning::Activate
    ) {
        Some(reporter)
    } else {
        None
    };

    let mut registry = RaceObjectRegistry::default();
    let admission =
        plan_item_operation(&registry, 1, &operation, reporter, owner_claim).map_err(|error| {
            ItemOperationServerAuditError::RegistryAdmission {
                detail: error.to_string(),
            }
        })?;
    let disposition = match admission {
        RaceObjectAdmission::PublishTracked(mutation) => {
            registry.commit(mutation).map_err(|error| {
                ItemOperationServerAuditError::RegistryAdmission {
                    detail: error.to_string(),
                }
            })?;
            ItemOperationAuditDisposition::PublishTracked
        }
        RaceObjectAdmission::PublishUntracked => ItemOperationAuditDisposition::PublishUntracked,
        RaceObjectAdmission::SuppressDuplicate { .. } => {
            ItemOperationAuditDisposition::SuppressDuplicate
        }
    };

    Ok(ItemOperationServerAudit {
        class_name: operation.schema.class_name,
        state: operation.state,
        disposition,
        relay_bytes: parsed.into_raw(),
    })
}

impl RaceObjectRegistry {
    pub(crate) fn plan(
        &self,
        operation: RaceObjectOperation,
    ) -> Result<RaceObjectAdmission, RaceObjectRegistryError> {
        let key = RaceObjectKey {
            race_epoch: operation.race_epoch,
            object_id: operation.object_id,
        };
        let current = self.objects.get(&key).copied();

        if matches!(
            operation.meaning,
            ItemLifecycleMeaning::Unknown | ItemLifecycleMeaning::NoClientAction
        ) {
            if current.is_some_and(|record| {
                record.class == operation.class && record.phase == RaceObjectPhase::Removed
            }) {
                let current = current.expect("checked Some above");
                return Ok(RaceObjectAdmission::SuppressDuplicate {
                    object_id: key.object_id,
                    class_name: current.class_name,
                    phase: current.phase,
                    state: operation.state,
                    kind: RaceObjectDuplicateKind::TerminalState,
                });
            }
            return Ok(RaceObjectAdmission::PublishUntracked);
        }

        if let Some(current) = current {
            Self::validate_existing(key, current, operation)?;
        }

        if current.is_some_and(|record| record.phase == RaceObjectPhase::Removed)
            && !matches!(
                operation.meaning,
                ItemLifecycleMeaning::Remove | ItemLifecycleMeaning::Respawn
            )
        {
            let current = current.expect("checked Some above");
            return Ok(RaceObjectAdmission::SuppressDuplicate {
                object_id: key.object_id,
                class_name: current.class_name,
                phase: current.phase,
                state: operation.state,
                kind: RaceObjectDuplicateKind::TerminalState,
            });
        }

        match operation.meaning {
            ItemLifecycleMeaning::Initialize
            | ItemLifecycleMeaning::Place
            | ItemLifecycleMeaning::Launch
            | ItemLifecycleMeaning::Activate => self.plan_install(key, current, operation),
            ItemLifecycleMeaning::Impact => self.plan_hit(key, current, operation),
            ItemLifecycleMeaning::Remove => self.plan_removal(key, current, operation),
            ItemLifecycleMeaning::Respawn => self.plan_respawn(key, current, operation),
            ItemLifecycleMeaning::UpdateRuntimeFlag => {
                Ok(self.plan_runtime_flag_update(key, current, operation))
            }
            // An exact writer shape does not prove lifecycle semantics. Relay
            // unresolved bodies without letting them erase an authoritative
            // hit/removal fingerprint on an already tracked object.
            ItemLifecycleMeaning::Unknown => Ok(RaceObjectAdmission::PublishUntracked),
            _ => Ok(self.plan_state_update(key, current, operation)),
        }
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "a planned mutation is a move-only commit capability and must not be reusable"
    )]
    pub(crate) fn commit(
        &mut self,
        mutation: RaceObjectMutation,
    ) -> Result<(), RaceObjectRegistryError> {
        if mutation.expected_revision != self.revision {
            return Err(RaceObjectRegistryError::StalePlan {
                planned_revision: mutation.expected_revision,
                current_revision: self.revision,
            });
        }
        self.objects.insert(mutation.key, mutation.next);
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn phase(&self, race_epoch: u64, object_id: u32) -> Option<RaceObjectPhase> {
        self.objects
            .get(&RaceObjectKey {
                race_epoch,
                object_id,
            })
            .map(|record| record.phase)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.objects.len()
    }

    fn plan_install(
        &self,
        key: RaceObjectKey,
        current: Option<RaceObjectRecord>,
        operation: RaceObjectOperation,
    ) -> Result<RaceObjectAdmission, RaceObjectRegistryError> {
        if let Some(current) = current {
            if current.phase == RaceObjectPhase::Initialized
                && current.last_meaning == ItemLifecycleMeaning::Initialize
                && matches!(
                    operation.meaning,
                    ItemLifecycleMeaning::Place
                        | ItemLifecycleMeaning::Launch
                        | ItemLifecycleMeaning::Activate
                )
            {
                let next =
                    self.next_record(key, Some(current), operation, RaceObjectPhase::Active)?;
                return Ok(RaceObjectAdmission::PublishTracked(RaceObjectMutation {
                    expected_revision: self.revision,
                    key,
                    next,
                    events: [Some(RaceObjectLifecycleEvent::Activated), None],
                }));
            }
            return Ok(RaceObjectAdmission::SuppressDuplicate {
                object_id: key.object_id,
                class_name: current.class_name,
                phase: current.phase,
                state: operation.state,
                kind: RaceObjectDuplicateKind::Install,
            });
        }
        self.ensure_capacity(key)?;
        let owner = operation.owner_claim.unwrap_or(operation.reporter);
        let (phase, events) = if operation.meaning == ItemLifecycleMeaning::Initialize {
            (
                RaceObjectPhase::Initialized,
                [Some(RaceObjectLifecycleEvent::Installed), None],
            )
        } else {
            (
                RaceObjectPhase::Active,
                [
                    Some(RaceObjectLifecycleEvent::Installed),
                    Some(RaceObjectLifecycleEvent::Activated),
                ],
            )
        };
        Ok(RaceObjectAdmission::PublishTracked(RaceObjectMutation {
            expected_revision: self.revision,
            key,
            next: RaceObjectRecord {
                class: operation.class,
                class_name: operation.class_name,
                owner,
                last_reporter: operation.reporter,
                phase,
                last_state: operation.state,
                last_meaning: operation.meaning,
                last_transition_token: operation.transition_token,
                last_source_object_id: operation.source_object_id,
                last_target_object_id: operation.target_object_id,
                runtime_flag: None,
            },
            events,
        }))
    }

    fn plan_respawn(
        &self,
        key: RaceObjectKey,
        current: Option<RaceObjectRecord>,
        operation: RaceObjectOperation,
    ) -> Result<RaceObjectAdmission, RaceObjectRegistryError> {
        let Some(current) = current else {
            // Respawn is a transition of an existing native item object. If
            // its installation was not observed, relay the proven body but do
            // not invent an owner or authoritative generation.
            return Ok(RaceObjectAdmission::PublishUntracked);
        };
        if current.phase == RaceObjectPhase::Active
            && current.last_meaning == ItemLifecycleMeaning::Respawn
            && current.last_state == operation.state
            && current.last_transition_token == operation.transition_token
            && current.last_source_object_id == operation.source_object_id
            && current.last_target_object_id == operation.target_object_id
        {
            return Ok(RaceObjectAdmission::SuppressDuplicate {
                object_id: key.object_id,
                class_name: current.class_name,
                phase: current.phase,
                state: operation.state,
                kind: RaceObjectDuplicateKind::Activation,
            });
        }
        let next = self.next_record(key, Some(current), operation, RaceObjectPhase::Active)?;
        Ok(RaceObjectAdmission::PublishTracked(RaceObjectMutation {
            expected_revision: self.revision,
            key,
            next,
            events: [Some(RaceObjectLifecycleEvent::Activated), None],
        }))
    }

    fn plan_hit(
        &self,
        key: RaceObjectKey,
        current: Option<RaceObjectRecord>,
        operation: RaceObjectOperation,
    ) -> Result<RaceObjectAdmission, RaceObjectRegistryError> {
        if current.is_some_and(|record| {
            record.phase == RaceObjectPhase::Removed
                || (record.phase == RaceObjectPhase::Hit
                    && record.last_state == operation.state
                    && record.last_transition_token == operation.transition_token
                    && record.last_target_object_id == operation.target_object_id)
        }) {
            let current = current.expect("checked Some above");
            return Ok(RaceObjectAdmission::SuppressDuplicate {
                object_id: key.object_id,
                class_name: current.class_name,
                phase: current.phase,
                state: operation.state,
                kind: RaceObjectDuplicateKind::Hit,
            });
        }
        let next = self.next_record(key, current, operation, RaceObjectPhase::Hit)?;
        Ok(RaceObjectAdmission::PublishTracked(RaceObjectMutation {
            expected_revision: self.revision,
            key,
            next,
            events: [Some(RaceObjectLifecycleEvent::Hit), None],
        }))
    }

    fn plan_removal(
        &self,
        key: RaceObjectKey,
        current: Option<RaceObjectRecord>,
        operation: RaceObjectOperation,
    ) -> Result<RaceObjectAdmission, RaceObjectRegistryError> {
        if current.is_some_and(|record| record.phase == RaceObjectPhase::Removed) {
            let current = current.expect("checked Some above");
            return Ok(RaceObjectAdmission::SuppressDuplicate {
                object_id: key.object_id,
                class_name: current.class_name,
                phase: current.phase,
                state: operation.state,
                kind: RaceObjectDuplicateKind::Removal,
            });
        }
        if current.is_none() && operation.evidence == ItemSemanticEvidence::StaticConsumer {
            // A consumer-only terminal branch (for example WaterbombFly state
            // 6 or SpeedDown state 2) does not prove that an object generation
            // was installed on this server. Relay it, but do not mint an
            // orphan tombstone.
            return Ok(RaceObjectAdmission::PublishUntracked);
        }
        let next = self.next_record(key, current, operation, RaceObjectPhase::Removed)?;
        Ok(RaceObjectAdmission::PublishTracked(RaceObjectMutation {
            expected_revision: self.revision,
            key,
            next,
            events: [Some(RaceObjectLifecycleEvent::Removed), None],
        }))
    }

    fn plan_state_update(
        &self,
        key: RaceObjectKey,
        current: Option<RaceObjectRecord>,
        operation: RaceObjectOperation,
    ) -> RaceObjectAdmission {
        let Some(current) = current else {
            // The strict codec proves the object/state shape, but no generic
            // lifecycle meaning is inferred for class-specific states 0/4+.
            return RaceObjectAdmission::PublishUntracked;
        };
        let mut next = current;
        next.last_reporter = operation.reporter;
        next.last_state = operation.state;
        next.last_meaning = operation.meaning;
        next.last_transition_token = operation.transition_token;
        next.last_source_object_id = operation.source_object_id;
        next.last_target_object_id = operation.target_object_id;
        RaceObjectAdmission::PublishTracked(RaceObjectMutation {
            expected_revision: self.revision,
            key,
            next,
            events: [
                Some(RaceObjectLifecycleEvent::StateUpdated(operation.state)),
                None,
            ],
        })
    }

    /// Record class-local state without replacing the last authoritative
    /// lifecycle transition. In particular, a `SpecialSmall` state-2 flag must
    /// not erase the state-1 hit fingerprint used for replay suppression.
    fn plan_runtime_flag_update(
        &self,
        key: RaceObjectKey,
        current: Option<RaceObjectRecord>,
        operation: RaceObjectOperation,
    ) -> RaceObjectAdmission {
        let Some(mut next) = current else {
            return RaceObjectAdmission::PublishUntracked;
        };
        let Some(runtime_flag) = operation.variant else {
            return RaceObjectAdmission::PublishUntracked;
        };
        if next.runtime_flag == Some(runtime_flag) {
            return RaceObjectAdmission::SuppressDuplicate {
                object_id: key.object_id,
                class_name: next.class_name,
                phase: next.phase,
                state: operation.state,
                kind: RaceObjectDuplicateKind::RuntimeFlagUpdate,
            };
        }
        next.last_reporter = operation.reporter;
        next.runtime_flag = Some(runtime_flag);
        RaceObjectAdmission::PublishTracked(RaceObjectMutation {
            expected_revision: self.revision,
            key,
            next,
            events: [
                Some(RaceObjectLifecycleEvent::RuntimeFlagUpdated(runtime_flag)),
                None,
            ],
        })
    }

    fn next_record(
        &self,
        key: RaceObjectKey,
        current: Option<RaceObjectRecord>,
        operation: RaceObjectOperation,
        phase: RaceObjectPhase,
    ) -> Result<RaceObjectRecord, RaceObjectRegistryError> {
        if let Some(mut current) = current {
            current.last_reporter = operation.reporter;
            current.phase = phase;
            current.last_state = operation.state;
            current.last_meaning = operation.meaning;
            current.last_transition_token = operation.transition_token;
            current.last_source_object_id = operation.source_object_id;
            current.last_target_object_id = operation.target_object_id;
            return Ok(current);
        }
        self.ensure_capacity(key)?;
        Ok(RaceObjectRecord {
            class: operation.class,
            class_name: operation.class_name,
            owner: operation.owner_claim.unwrap_or(operation.reporter),
            last_reporter: operation.reporter,
            phase,
            last_state: operation.state,
            last_meaning: operation.meaning,
            last_transition_token: operation.transition_token,
            last_source_object_id: operation.source_object_id,
            last_target_object_id: operation.target_object_id,
            runtime_flag: None,
        })
    }

    fn validate_existing(
        key: RaceObjectKey,
        current: RaceObjectRecord,
        operation: RaceObjectOperation,
    ) -> Result<(), RaceObjectRegistryError> {
        if current.class != operation.class {
            return Err(RaceObjectRegistryError::ClassMismatch {
                race_epoch: key.race_epoch,
                object_id: key.object_id,
                expected_operation_hash: current.class.operation_hash,
                expected_base_hash: current.class.base_hash,
                actual_operation_hash: operation.class.operation_hash,
                actual_base_hash: operation.class.base_hash,
            });
        }
        if let Some(owner) = operation.owner_claim
            && owner != current.owner
        {
            return Err(RaceObjectRegistryError::OwnerMismatch {
                race_epoch: key.race_epoch,
                object_id: key.object_id,
                expected_player_id: current.owner.player_id,
                expected_generation: current.owner.generation,
                actual_player_id: owner.player_id,
                actual_generation: owner.generation,
            });
        }
        Ok(())
    }

    fn ensure_capacity(&self, key: RaceObjectKey) -> Result<(), RaceObjectRegistryError> {
        if self.objects.len() >= MAX_RACE_OBJECTS {
            return Err(RaceObjectRegistryError::Capacity {
                race_epoch: key.race_epoch,
                object_id: key.object_id,
                maximum: MAX_RACE_OBJECTS,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use p5136_core::{
        game_slot_item_semantics::{ItemLifecycleMeaning, ItemSemanticEvidence},
        game_slot_protocol::{GAME_SLOT_PACKET_HASH, GameSlotBody, parse_game_slot_packet},
    };

    use super::{
        RaceObjectActor, RaceObjectAdmission, RaceObjectClass, RaceObjectDuplicateKind,
        RaceObjectLifecycleEvent, RaceObjectOperation, RaceObjectPhase, RaceObjectRegistry,
        RaceObjectRegistryError,
    };

    const CLASS: RaceObjectClass = RaceObjectClass {
        operation_hash: 0x1D86_04A3,
        base_hash: 0x2D06_05C2,
    };
    const OWNER: RaceObjectActor = RaceObjectActor {
        user_no: 10,
        generation: 1,
        player_id: 0,
    };
    const VICTIM: RaceObjectActor = RaceObjectActor {
        user_no: 15,
        generation: 4,
        player_id: 1,
    };

    fn operation(state: u32, reporter: RaceObjectActor) -> RaceObjectOperation {
        RaceObjectOperation {
            race_epoch: 7,
            class: CLASS,
            class_name: "GopBarricade",
            object_id: 0x1000_0048,
            state,
            meaning: match state {
                0 => ItemLifecycleMeaning::Initialize,
                1 => ItemLifecycleMeaning::Place,
                2 => ItemLifecycleMeaning::Impact,
                4 => ItemLifecycleMeaning::Remove,
                _ => ItemLifecycleMeaning::Resolve,
            },
            evidence: ItemSemanticEvidence::ProducerAndConsumer,
            transition_token: Some(0xAB00_0000 | state),
            source_object_id: Some(0),
            target_object_id: Some(1),
            variant: None,
            reporter,
            owner_claim: Some(OWNER),
        }
    }

    #[test]
    fn semantic_install_hit_remove_is_generation_bound_and_exact_hit_is_suppressed() {
        let mut registry = RaceObjectRegistry::default();
        let RaceObjectAdmission::PublishTracked(install) =
            registry.plan(operation(1, OWNER)).unwrap()
        else {
            panic!("install must be tracked");
        };
        assert_eq!(
            install.events().collect::<Vec<_>>(),
            [
                RaceObjectLifecycleEvent::Installed,
                RaceObjectLifecycleEvent::Activated
            ]
        );
        assert_eq!(registry.phase(7, 0x1000_0048), None);
        registry.commit(install).unwrap();
        assert_eq!(
            registry.phase(7, 0x1000_0048),
            Some(RaceObjectPhase::Active)
        );

        let RaceObjectAdmission::PublishTracked(hit) = registry.plan(operation(2, VICTIM)).unwrap()
        else {
            panic!("first hit must be tracked");
        };
        assert_eq!(hit.owner(), OWNER);
        assert_eq!(hit.reporter(), VICTIM);
        assert_eq!(
            hit.events().collect::<Vec<_>>(),
            [RaceObjectLifecycleEvent::Hit]
        );
        registry.commit(hit).unwrap();

        assert!(matches!(
            registry.plan(operation(2, OWNER)).unwrap(),
            RaceObjectAdmission::SuppressDuplicate {
                kind: RaceObjectDuplicateKind::Hit,
                ..
            }
        ));

        let RaceObjectAdmission::PublishTracked(removal) =
            registry.plan(operation(4, VICTIM)).unwrap()
        else {
            panic!("removal must be tracked");
        };
        registry.commit(removal).unwrap();
        assert_eq!(
            registry.phase(7, 0x1000_0048),
            Some(RaceObjectPhase::Removed)
        );
        assert_eq!(registry.len(), 1);

        let mut next_race = operation(1, OWNER);
        next_race.race_epoch = 8;
        let RaceObjectAdmission::PublishTracked(next_install) = registry.plan(next_race).unwrap()
        else {
            panic!("a new race epoch may reuse an object ID");
        };
        registry.commit(next_install).unwrap();
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn object_class_and_native_owner_claim_cannot_change() {
        let mut registry = RaceObjectRegistry::default();
        let RaceObjectAdmission::PublishTracked(install) =
            registry.plan(operation(1, OWNER)).unwrap()
        else {
            panic!("install must be tracked");
        };
        registry.commit(install).unwrap();

        let mut changed_class = operation(2, VICTIM);
        changed_class.class.operation_hash ^= 1;
        assert!(matches!(
            registry.plan(changed_class),
            Err(RaceObjectRegistryError::ClassMismatch { .. })
        ));

        let mut changed_owner = operation(2, VICTIM);
        changed_owner.owner_claim = Some(VICTIM);
        assert!(matches!(
            registry.plan(changed_owner),
            Err(RaceObjectRegistryError::OwnerMismatch { .. })
        ));
    }

    #[test]
    fn transition_without_seen_install_is_still_bounded_and_deduplicated() {
        let mut registry = RaceObjectRegistry::default();
        let mut orphan = operation(4, VICTIM);
        orphan.object_id = 0x2000_004E;
        let RaceObjectAdmission::PublishTracked(removal) = registry.plan(orphan).unwrap() else {
            panic!("captured orphan transition must get a tombstone");
        };
        registry.commit(removal).unwrap();
        assert!(matches!(
            registry.plan(orphan).unwrap(),
            RaceObjectAdmission::SuppressDuplicate {
                kind: RaceObjectDuplicateKind::Removal,
                ..
            }
        ));
    }

    #[test]
    fn resolve_state_after_hit_is_not_misclassified_as_removal() {
        let mut registry = RaceObjectRegistry::default();
        let RaceObjectAdmission::PublishTracked(install) =
            registry.plan(operation(1, OWNER)).unwrap()
        else {
            panic!("install must be tracked");
        };
        registry.commit(install).unwrap();

        let RaceObjectAdmission::PublishTracked(hit) = registry.plan(operation(2, VICTIM)).unwrap()
        else {
            panic!("hit must be tracked");
        };
        registry.commit(hit).unwrap();

        let RaceObjectAdmission::PublishTracked(resolve) =
            registry.plan(operation(3, VICTIM)).unwrap()
        else {
            panic!("state 3 resolve must remain publishable");
        };
        assert_eq!(
            resolve.events().collect::<Vec<_>>(),
            [RaceObjectLifecycleEvent::StateUpdated(3)]
        );
        assert_eq!(resolve.phase(), RaceObjectPhase::Hit);
    }

    #[test]
    fn initialize_then_place_publishes_both_distinct_native_transitions() {
        let mut registry = RaceObjectRegistry::default();
        let RaceObjectAdmission::PublishTracked(initialize) =
            registry.plan(operation(0, OWNER)).unwrap()
        else {
            panic!("initialization must be tracked");
        };
        assert_eq!(initialize.phase(), RaceObjectPhase::Initialized);
        assert_eq!(
            initialize.events().collect::<Vec<_>>(),
            [RaceObjectLifecycleEvent::Installed]
        );
        registry.commit(initialize).unwrap();

        let RaceObjectAdmission::PublishTracked(place) =
            registry.plan(operation(1, OWNER)).unwrap()
        else {
            panic!("placement after initialization must remain publishable");
        };
        assert_eq!(place.phase(), RaceObjectPhase::Active);
        assert_eq!(
            place.events().collect::<Vec<_>>(),
            [RaceObjectLifecycleEvent::Activated]
        );
        registry.commit(place).unwrap();

        assert!(matches!(
            registry.plan(operation(1, OWNER)).unwrap(),
            RaceObjectAdmission::SuppressDuplicate {
                kind: RaceObjectDuplicateKind::Install,
                ..
            }
        ));
    }

    #[test]
    fn mine_remove_respawn_reactivates_the_same_object_for_later_impacts() {
        const PAIR: (u32, u32) = (0x0A6B_02AF, 0x1450_03CE);
        const OBJECT_ID: u32 = 0x7000_0006;
        fn mine_wire(state: u32, length: usize) -> Vec<u8> {
            let mut raw = vec![0_u8; length];
            raw[0..4].copy_from_slice(&PAIR.0.to_le_bytes());
            raw[4..8].copy_from_slice(&PAIR.1.to_le_bytes());
            raw[8..12].copy_from_slice(&OBJECT_ID.to_le_bytes());
            raw[12..16].copy_from_slice(&state.to_le_bytes());
            raw[16..20].copy_from_slice(&(0x7100_0000 | state).to_le_bytes());
            match state {
                1 => {
                    raw[72] = 1;
                    raw[73..77].copy_from_slice(&0x7200_0000_u32.to_le_bytes());
                }
                2 => {
                    raw[20..24].copy_from_slice(&0x7300_0002_u32.to_le_bytes());
                    raw[24] = 2;
                    raw[25..29].copy_from_slice(&0x7200_0002_u32.to_le_bytes());
                }
                5 => {
                    raw[20..24].copy_from_slice(&0x7200_0005_u32.to_le_bytes());
                    raw[24] = 5;
                }
                6 => {}
                _ => panic!("unsupported Mine test state"),
            }
            let mut wire = vec![0_u8; 20 + raw.len()];
            wire[0..4].copy_from_slice(&GAME_SLOT_PACKET_HASH.to_le_bytes());
            wire[8..12].copy_from_slice(&2_u32.to_le_bytes());
            wire[12] = 12;
            wire[16..20].copy_from_slice(&u32::try_from(raw.len()).unwrap().to_le_bytes());
            wire[20..].copy_from_slice(&raw);
            wire
        }

        fn parsed_operation(
            state: u32,
            length: usize,
            reporter: RaceObjectActor,
        ) -> RaceObjectOperation {
            let parsed = parse_game_slot_packet(&mine_wire(state, length)).unwrap();
            let GameSlotBody::ItemOperation(item) = parsed.body() else {
                panic!("Mine state {state} did not reach strict parsing");
            };
            RaceObjectOperation {
                race_epoch: 7,
                class: RaceObjectClass {
                    operation_hash: item.operation_hash,
                    base_hash: item.operation_base_hash,
                },
                class_name: item.schema.class_name,
                object_id: item.object_id,
                state: item.state,
                meaning: item.semantics.meaning,
                evidence: item.semantics.evidence,
                transition_token: item.semantics.transition_token,
                source_object_id: item.semantics.source_object_id,
                target_object_id: item.semantics.target_object_id,
                variant: item.semantics.variant,
                reporter,
                owner_claim: (state == 1).then_some(OWNER),
            }
        }

        let mut registry = RaceObjectRegistry::default();
        let RaceObjectAdmission::PublishTracked(install) =
            registry.plan(parsed_operation(1, 77, OWNER)).unwrap()
        else {
            panic!("install must be tracked");
        };
        registry.commit(install).unwrap();

        let RaceObjectAdmission::PublishTracked(removal) =
            registry.plan(parsed_operation(5, 29, OWNER)).unwrap()
        else {
            panic!("removal must be tracked");
        };
        registry.commit(removal).unwrap();
        assert_eq!(registry.phase(7, OBJECT_ID), Some(RaceObjectPhase::Removed));

        let RaceObjectAdmission::PublishTracked(respawn) =
            registry.plan(parsed_operation(6, 68, OWNER)).unwrap()
        else {
            panic!("respawn must reactivate the tracked object");
        };
        assert_eq!(respawn.phase(), RaceObjectPhase::Active);
        registry.commit(respawn).unwrap();

        let RaceObjectAdmission::PublishTracked(hit) =
            registry.plan(parsed_operation(2, 29, VICTIM)).unwrap()
        else {
            panic!("impact after respawn must remain publishable");
        };
        assert_eq!(hit.phase(), RaceObjectPhase::Hit);
    }

    #[test]
    fn unknown_semantics_never_mutate_a_tracked_fingerprint_and_terminals_stay_closed() {
        let mut registry = RaceObjectRegistry::default();
        let RaceObjectAdmission::PublishTracked(install) =
            registry.plan(operation(1, OWNER)).unwrap()
        else {
            panic!("install must be tracked");
        };
        registry.commit(install).unwrap();
        let RaceObjectAdmission::PublishTracked(hit) = registry.plan(operation(2, VICTIM)).unwrap()
        else {
            panic!("hit must be tracked");
        };
        registry.commit(hit).unwrap();

        let mut unknown = operation(99, VICTIM);
        unknown.meaning = ItemLifecycleMeaning::Unknown;
        unknown.transition_token = None;
        unknown.source_object_id = None;
        unknown.target_object_id = None;
        assert_eq!(
            registry.plan(unknown).unwrap(),
            RaceObjectAdmission::PublishUntracked
        );
        let mut colliding_unknown = unknown;
        colliding_unknown.class.operation_hash ^= 1;
        colliding_unknown.owner_claim = Some(VICTIM);
        assert_eq!(
            registry.plan(colliding_unknown).unwrap(),
            RaceObjectAdmission::PublishUntracked
        );
        let mut no_client_action = unknown;
        no_client_action.meaning = ItemLifecycleMeaning::NoClientAction;
        assert_eq!(
            registry.plan(no_client_action).unwrap(),
            RaceObjectAdmission::PublishUntracked
        );
        assert!(matches!(
            registry.plan(operation(2, OWNER)).unwrap(),
            RaceObjectAdmission::SuppressDuplicate {
                kind: RaceObjectDuplicateKind::Hit,
                ..
            }
        ));

        let mut removal = operation(4, VICTIM);
        removal.meaning = ItemLifecycleMeaning::Remove;
        let RaceObjectAdmission::PublishTracked(removal) = registry.plan(removal).unwrap() else {
            panic!("removal must be tracked");
        };
        registry.commit(removal).unwrap();
        assert!(matches!(
            registry.plan(unknown).unwrap(),
            RaceObjectAdmission::SuppressDuplicate {
                kind: RaceObjectDuplicateKind::TerminalState,
                ..
            }
        ));
        assert!(matches!(
            registry.plan(operation(3, VICTIM)).unwrap(),
            RaceObjectAdmission::SuppressDuplicate {
                kind: RaceObjectDuplicateKind::TerminalState,
                ..
            }
        ));
    }

    #[test]
    fn consumer_only_removal_cannot_mint_an_unseen_object_tombstone() {
        const PAIR: (u32, u32) = (0x2EE3_05F4, 0x41C0_0713);
        const OBJECT_ID: u32 = 0x7000_0066;
        fn wire(state: u32, length: usize) -> Vec<u8> {
            let mut wire = vec![0_u8; 20 + length];
            wire[0..4].copy_from_slice(&GAME_SLOT_PACKET_HASH.to_le_bytes());
            wire[8..12].copy_from_slice(&2_u32.to_le_bytes());
            wire[12] = 12;
            wire[16..20].copy_from_slice(&u32::try_from(length).unwrap().to_le_bytes());
            wire[20..24].copy_from_slice(&PAIR.0.to_le_bytes());
            wire[24..28].copy_from_slice(&PAIR.1.to_le_bytes());
            wire[28..32].copy_from_slice(&OBJECT_ID.to_le_bytes());
            wire[32..36].copy_from_slice(&state.to_le_bytes());
            wire
        }

        let terminal_wire = wire(6, 16);
        let parsed = parse_game_slot_packet(&terminal_wire).unwrap();
        let GameSlotBody::ItemOperation(terminal_item) = parsed.body() else {
            panic!("WaterbombFly state 6 must reach strict parsing");
        };
        assert_eq!(
            terminal_item.semantics.meaning,
            ItemLifecycleMeaning::Remove
        );
        assert_eq!(
            terminal_item.semantics.evidence,
            ItemSemanticEvidence::StaticConsumer
        );
        assert_eq!(terminal_item.semantics.transition_token, None);
        assert_eq!(terminal_item.semantics.source_object_id, None);
        assert_eq!(terminal_item.semantics.target_object_id, None);
        let terminal = RaceObjectOperation {
            race_epoch: 7,
            class: RaceObjectClass {
                operation_hash: terminal_item.operation_hash,
                base_hash: terminal_item.operation_base_hash,
            },
            class_name: terminal_item.schema.class_name,
            object_id: terminal_item.object_id,
            state: terminal_item.state,
            meaning: terminal_item.semantics.meaning,
            evidence: terminal_item.semantics.evidence,
            transition_token: terminal_item.semantics.transition_token,
            source_object_id: terminal_item.semantics.source_object_id,
            target_object_id: terminal_item.semantics.target_object_id,
            variant: terminal_item.semantics.variant,
            reporter: OWNER,
            owner_claim: None,
        };

        let mut registry = RaceObjectRegistry::default();
        assert_eq!(
            registry.plan(terminal).unwrap(),
            RaceObjectAdmission::PublishUntracked
        );
        assert_eq!(registry.len(), 0);

        let install_wire = wire(1, 77);
        let parsed = parse_game_slot_packet(&install_wire).unwrap();
        let GameSlotBody::ItemOperation(install_item) = parsed.body() else {
            panic!("WaterbombFly state 1 must reach strict parsing");
        };
        let RaceObjectAdmission::PublishTracked(install) = registry
            .plan(RaceObjectOperation {
                race_epoch: 7,
                class: terminal.class,
                class_name: install_item.schema.class_name,
                object_id: install_item.object_id,
                state: install_item.state,
                meaning: install_item.semantics.meaning,
                evidence: install_item.semantics.evidence,
                transition_token: install_item.semantics.transition_token,
                source_object_id: install_item.semantics.source_object_id,
                target_object_id: install_item.semantics.target_object_id,
                variant: install_item.semantics.variant,
                reporter: OWNER,
                owner_claim: Some(OWNER),
            })
            .unwrap()
        else {
            panic!("install must be tracked");
        };
        registry.commit(install).unwrap();
        let RaceObjectAdmission::PublishTracked(removal) = registry.plan(terminal).unwrap() else {
            panic!("the consumer-only terminal may close an observed object");
        };
        assert_eq!(removal.phase(), RaceObjectPhase::Removed);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one table proves every reconstructed class from the outer wire through registry admission"
    )]
    fn all_fifteen_contracts_cross_game_slot_parse_semantics_and_registry_admission() {
        #[derive(Clone, Copy)]
        struct Case {
            class_name: &'static str,
            pair: (u32, u32),
            state: u32,
            length: usize,
            meaning: ItemLifecycleMeaning,
            phase: Option<u8>,
            token_offset: usize,
            source_offset: usize,
            target_offset: usize,
            variant_offset: Option<usize>,
            barricade_actor_fields: bool,
        }

        fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }

        let mut cases = vec![
            Case {
                class_name: "GopBarricade",
                pair: (0x1D86_04A3, 0x2D06_05C2),
                state: 2,
                length: 25,
                meaning: ItemLifecycleMeaning::Impact,
                phase: Some(3),
                token_offset: 13,
                source_offset: 17,
                target_offset: 21,
                variant_offset: None,
                barricade_actor_fields: true,
            },
            Case {
                class_name: "GopBanana",
                pair: (0x1090_0367, 0x1CB3_0486),
                state: 2,
                length: 30,
                meaning: ItemLifecycleMeaning::Impact,
                phase: None,
                token_offset: 16,
                source_offset: 25,
                target_offset: 20,
                variant_offset: Some(24),
                barricade_actor_fields: false,
            },
            Case {
                class_name: "GopMine",
                pair: (0x0A6B_02AF, 0x1450_03CE),
                state: 2,
                length: 29,
                meaning: ItemLifecycleMeaning::Impact,
                phase: None,
                token_offset: 16,
                source_offset: 25,
                target_offset: 20,
                variant_offset: Some(24),
                barricade_actor_fields: false,
            },
            Case {
                class_name: "GopRocket",
                pair: (0x1129_038E, 0x1D4C_04AD),
                state: 1,
                length: 82,
                meaning: ItemLifecycleMeaning::Launch,
                phase: Some(1),
                token_offset: 18,
                source_offset: 22,
                target_offset: 75,
                variant_offset: Some(26),
                barricade_actor_fields: false,
            },
        ];
        for (class_name, pair) in [
            ("GopCokeRocket", (0x2261_0510, 0x3300_062F)),
            ("GopGoldRocket", (0x228A_0514, 0x3329_0633)),
            ("GopDinoClawRocket", (0x3A06_069F, 0x4F21_07BE)),
            ("GopTigerRocket", (0x2882_0589, 0x3A40_06A8)),
            ("GopSnowman", (0x1584_0409, 0x22C6_0528)),
        ] {
            cases.push(Case {
                class_name,
                pair,
                state: 1,
                length: 77,
                meaning: ItemLifecycleMeaning::Launch,
                phase: Some(1),
                token_offset: 16,
                source_offset: 20,
                target_offset: 73,
                variant_offset: Some(24),
                barricade_actor_fields: false,
            });
        }
        cases.extend([
            Case {
                class_name: "GopSuperMag",
                pair: (0x198F_044A, 0x27F0_0569),
                state: 1,
                length: 29,
                meaning: ItemLifecycleMeaning::Activate,
                phase: Some(0),
                token_offset: 16,
                source_offset: 20,
                target_offset: 24,
                variant_offset: Some(28),
                barricade_actor_fields: false,
            },
            Case {
                class_name: "GopWaterbomb",
                pair: (0x1E65_04C9, 0x2DE5_05E8),
                state: 2,
                length: 29,
                meaning: ItemLifecycleMeaning::Impact,
                phase: Some(2),
                token_offset: 16,
                source_offset: 25,
                target_offset: 20,
                variant_offset: Some(24),
                barricade_actor_fields: false,
            },
        ]);
        for (class_name, pair) in [
            ("GopWaterfly", (0x19AE_0474, 0x280F_0593)),
            ("GopInfectedWaterfly", (0x49AB_0796, 0x6104_08B5)),
            ("GopSnowWaterfly", (0x2F69_061B, 0x4246_073A)),
            ("GopWaterbombFly", (0x2EE3_05F4, 0x41C0_0713)),
        ] {
            cases.push(Case {
                class_name,
                pair,
                state: 1,
                length: 77,
                meaning: ItemLifecycleMeaning::Launch,
                phase: Some(0),
                token_offset: 16,
                source_offset: 20,
                target_offset: 24,
                variant_offset: Some(28),
                barricade_actor_fields: false,
            });
        }
        assert_eq!(cases.len(), 15);

        let mut registry = RaceObjectRegistry::default();
        for (index, case) in cases.iter().copied().enumerate() {
            let index = u32::try_from(index).unwrap();
            let object_id = 0x7000_0000 | index;
            let token = 0x7100_0000 | index;
            let source = if case.barricade_actor_fields {
                0
            } else {
                0x7200_0000 | index
            };
            let target = if case.barricade_actor_fields {
                0
            } else {
                0x7300_0000 | index
            };
            let variant = u8::try_from(index + 1).unwrap();
            let mut raw = vec![0_u8; case.length];
            put_u32(&mut raw, 0, case.pair.0);
            put_u32(&mut raw, 4, case.pair.1);
            put_u32(&mut raw, 8, object_id);
            if case.barricade_actor_fields {
                raw[12] = u8::try_from(case.state).unwrap();
            } else {
                put_u32(&mut raw, 12, case.state);
            }
            put_u32(&mut raw, case.token_offset, token);
            put_u32(&mut raw, case.source_offset, source);
            put_u32(&mut raw, case.target_offset, target);
            if let Some(offset) = case.variant_offset {
                raw[offset] = variant;
            }

            let mut wire = vec![0_u8; 20 + raw.len()];
            put_u32(&mut wire, 0, GAME_SLOT_PACKET_HASH);
            put_u32(&mut wire, 4, 0);
            put_u32(&mut wire, 8, 2);
            wire[12] = 12;
            put_u32(&mut wire, 16, u32::try_from(raw.len()).unwrap());
            wire[20..].copy_from_slice(&raw);

            let parsed = parse_game_slot_packet(&wire).unwrap();
            let GameSlotBody::ItemOperation(item) = parsed.body() else {
                panic!("{} did not reach strict parsing", case.class_name);
            };
            assert_eq!(item.schema.class_name, case.class_name);
            assert_eq!((item.operation_hash, item.operation_base_hash), case.pair);
            assert_eq!(item.object_id, object_id);
            assert_eq!(item.state, case.state);
            assert_eq!(item.semantics.meaning, case.meaning);
            assert_eq!(item.semantics.native_phase, case.phase);
            assert_eq!(item.semantics.transition_token, Some(token));
            assert_eq!(item.semantics.source_object_id, Some(source));
            assert_eq!(item.semantics.target_object_id, Some(target));
            assert_eq!(item.semantics.variant, case.variant_offset.map(|_| variant));

            let admission = registry
                .plan(RaceObjectOperation {
                    race_epoch: u64::from(index) + 100,
                    class: RaceObjectClass {
                        operation_hash: item.operation_hash,
                        base_hash: item.operation_base_hash,
                    },
                    class_name: item.schema.class_name,
                    object_id: item.object_id,
                    state: item.state,
                    meaning: item.semantics.meaning,
                    evidence: item.semantics.evidence,
                    transition_token: item.semantics.transition_token,
                    source_object_id: item.semantics.source_object_id,
                    target_object_id: item.semantics.target_object_id,
                    variant: item.semantics.variant,
                    reporter: OWNER,
                    owner_claim: (case.barricade_actor_fields
                        || matches!(
                            case.meaning,
                            ItemLifecycleMeaning::Initialize
                                | ItemLifecycleMeaning::Place
                                | ItemLifecycleMeaning::Launch
                                | ItemLifecycleMeaning::Activate
                        ))
                    .then_some(OWNER),
                })
                .unwrap();
            let RaceObjectAdmission::PublishTracked(mutation) = admission else {
                panic!("{} did not reach tracked admission", case.class_name);
            };
            assert_eq!(mutation.meaning(), case.meaning);
            assert_eq!(mutation.transition_token(), Some(token));
            assert_eq!(mutation.source_object_id(), Some(source));
            assert_eq!(mutation.target_object_id(), Some(target));
            assert_eq!(
                mutation.phase(),
                if case.meaning == ItemLifecycleMeaning::Impact {
                    RaceObjectPhase::Hit
                } else {
                    RaceObjectPhase::Active
                }
            );
            registry.commit(mutation).unwrap();
        }
        assert_eq!(registry.len(), cases.len());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the literal client-consumer table is intentionally independent of the production schema"
    )]
    fn second_pass_literal_states_cross_wire_semantics_and_registry() {
        #[derive(Clone, Copy)]
        struct Case {
            name: &'static str,
            pair: (u32, u32),
            object_id: u32,
            state: u32,
            length: usize,
            state_offset: usize,
            token_offset: Option<usize>,
            source_offset: Option<usize>,
            target_offset: Option<usize>,
            variant: Option<(usize, u8)>,
            expected_variant: Option<u8>,
            meaning: ItemLifecycleMeaning,
            native_phase: Option<u8>,
            registry_phase: RaceObjectPhase,
        }

        fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }

        const COKE: (u32, u32) = (0x1900_0448, 0x2761_0567);
        const SNOW: (u32, u32) = (0x19EB_046D, 0x284C_058C);
        const INFECTED: (u32, u32) = (0x2DC1_05C8, 0x409E_06E7);
        const ROLLING_COKE: (u32, u32) = (0x42E4_071F, 0x591E_083E);
        const ROLLING_INFECTED: (u32, u32) = (0x6381_08BF, 0x7E37_09DE);
        const WATER_MINE: (u32, u32) = (0x1E04_04B2, 0x2D84_05D1);
        const TIME_MINE: (u32, u32) = (0x1909_043E, 0x276A_055D);

        macro_rules! case {
            ($name:literal, $pair:expr, $object:expr, $state:expr, $length:expr,
             $state_offset:expr, $token:expr, $source:expr, $target:expr,
             $variant:expr, $expected_variant:expr, $meaning:expr, $native:expr,
             $registry:expr) => {
                Case {
                    name: $name,
                    pair: $pair,
                    object_id: $object,
                    state: $state,
                    length: $length,
                    state_offset: $state_offset,
                    token_offset: $token,
                    source_offset: $source,
                    target_offset: $target,
                    variant: $variant,
                    expected_variant: $expected_variant,
                    meaning: $meaning,
                    native_phase: $native,
                    registry_phase: $registry,
                }
            };
        }

        let cases = [
            case!(
                "GopCokebomb",
                COKE,
                0x7000_0101,
                1,
                120,
                12,
                Some(16),
                Some(20),
                None,
                None,
                None,
                ItemLifecycleMeaning::Launch,
                Some(0),
                RaceObjectPhase::Active
            ),
            case!(
                "GopCokebomb",
                COKE,
                0x7000_0101,
                2,
                28,
                12,
                Some(16),
                Some(24),
                Some(20),
                None,
                None,
                ItemLifecycleMeaning::Impact,
                Some(2),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopCokebomb",
                COKE,
                0x7000_0101,
                3,
                28,
                12,
                Some(16),
                Some(20),
                Some(20),
                None,
                None,
                ItemLifecycleMeaning::Resolve,
                Some(3),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopCokebomb",
                COKE,
                0x7000_0101,
                4,
                28,
                12,
                Some(16),
                Some(24),
                Some(20),
                None,
                None,
                ItemLifecycleMeaning::Resolve,
                Some(4),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopSnowbomb",
                SNOW,
                0x7000_0102,
                1,
                120,
                12,
                Some(16),
                Some(20),
                None,
                None,
                None,
                ItemLifecycleMeaning::Launch,
                Some(0),
                RaceObjectPhase::Active
            ),
            case!(
                "GopSnowbomb",
                SNOW,
                0x7000_0102,
                2,
                28,
                12,
                Some(16),
                Some(24),
                Some(20),
                None,
                None,
                ItemLifecycleMeaning::Impact,
                Some(2),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopSnowbomb",
                SNOW,
                0x7000_0102,
                3,
                28,
                12,
                Some(16),
                Some(20),
                Some(20),
                None,
                None,
                ItemLifecycleMeaning::Resolve,
                Some(3),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopSnowbomb",
                SNOW,
                0x7000_0102,
                4,
                28,
                12,
                Some(16),
                Some(20),
                Some(20),
                None,
                None,
                ItemLifecycleMeaning::Resolve,
                Some(4),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopInfectedBomb",
                INFECTED,
                0x7000_0103,
                1,
                121,
                12,
                Some(16),
                Some(20),
                None,
                Some((120, 7)),
                Some(7),
                ItemLifecycleMeaning::Launch,
                Some(0),
                RaceObjectPhase::Active
            ),
            case!(
                "GopInfectedBomb",
                INFECTED,
                0x7000_0103,
                2,
                33,
                12,
                Some(16),
                Some(29),
                Some(20),
                Some((28, 7)),
                Some(7),
                ItemLifecycleMeaning::Impact,
                Some(2),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopInfectedBomb",
                INFECTED,
                0x7000_0103,
                3,
                33,
                12,
                Some(16),
                Some(29),
                Some(20),
                Some((28, 7)),
                Some(7),
                ItemLifecycleMeaning::Resolve,
                Some(4),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopRollingCokebomb",
                ROLLING_COKE,
                0x7000_0104,
                1,
                132,
                12,
                Some(16),
                Some(20),
                None,
                None,
                None,
                ItemLifecycleMeaning::Launch,
                Some(0),
                RaceObjectPhase::Active
            ),
            case!(
                "GopRollingCokebomb",
                ROLLING_COKE,
                0x7000_0104,
                2,
                28,
                12,
                Some(16),
                Some(24),
                Some(20),
                None,
                None,
                ItemLifecycleMeaning::Impact,
                Some(2),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopRollingCokebomb",
                ROLLING_COKE,
                0x7000_0104,
                3,
                24,
                12,
                Some(16),
                Some(20),
                Some(20),
                None,
                None,
                ItemLifecycleMeaning::Resolve,
                Some(3),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopRollingCokebomb",
                ROLLING_COKE,
                0x7000_0104,
                4,
                24,
                12,
                Some(16),
                Some(20),
                Some(20),
                None,
                None,
                ItemLifecycleMeaning::Resolve,
                Some(4),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopRollingInfectedbomb",
                ROLLING_INFECTED,
                0x7000_0105,
                1,
                132,
                12,
                Some(16),
                Some(20),
                None,
                None,
                None,
                ItemLifecycleMeaning::Launch,
                Some(0),
                RaceObjectPhase::Active
            ),
            case!(
                "GopRollingInfectedbomb",
                ROLLING_INFECTED,
                0x7000_0105,
                2,
                32,
                12,
                Some(16),
                Some(28),
                Some(20),
                None,
                None,
                ItemLifecycleMeaning::Impact,
                Some(2),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopRollingInfectedbomb",
                ROLLING_INFECTED,
                0x7000_0105,
                3,
                28,
                12,
                Some(16),
                None,
                Some(20),
                None,
                None,
                ItemLifecycleMeaning::Resolve,
                Some(4),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopWaterMine",
                WATER_MINE,
                0x7000_0106,
                1,
                73,
                12,
                Some(16),
                Some(68),
                None,
                Some((72, 7)),
                Some(7),
                ItemLifecycleMeaning::Place,
                Some(0),
                RaceObjectPhase::Active
            ),
            case!(
                "GopWaterMine",
                WATER_MINE,
                0x7000_0106,
                2,
                29,
                12,
                Some(16),
                Some(24),
                Some(20),
                Some((28, 7)),
                Some(7),
                ItemLifecycleMeaning::Impact,
                Some(2),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopWaterMine",
                WATER_MINE,
                0x7000_0106,
                3,
                29,
                12,
                Some(16),
                Some(24),
                Some(20),
                Some((28, 7)),
                Some(7),
                ItemLifecycleMeaning::Resolve,
                Some(3),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopWaterMine",
                WATER_MINE,
                0x7000_0106,
                4,
                29,
                12,
                Some(16),
                Some(24),
                Some(20),
                Some((28, 7)),
                Some(7),
                ItemLifecycleMeaning::Resolve,
                Some(4),
                RaceObjectPhase::Hit
            ),
            case!(
                "GopWaterMine",
                WATER_MINE,
                0x7000_0106,
                7,
                29,
                12,
                None,
                None,
                None,
                Some((28, 7)),
                None,
                ItemLifecycleMeaning::NoClientAction,
                None,
                RaceObjectPhase::Hit
            ),
            case!(
                "GopTimeMine",
                TIME_MINE,
                0x7000_0107,
                1,
                85,
                16,
                Some(20),
                Some(81),
                None,
                Some((80, 7)),
                Some(7),
                ItemLifecycleMeaning::Place,
                Some(0),
                RaceObjectPhase::Active
            ),
            case!(
                "GopTimeMine",
                TIME_MINE,
                0x7000_0107,
                2,
                33,
                16,
                Some(20),
                None,
                None,
                Some((28, 0)),
                Some(0),
                ItemLifecycleMeaning::Resolve,
                None,
                RaceObjectPhase::Active
            ),
            case!(
                "GopTimeMine",
                TIME_MINE,
                0x7000_0107,
                2,
                33,
                16,
                Some(20),
                None,
                Some(24),
                Some((28, 1)),
                Some(1),
                ItemLifecycleMeaning::Impact,
                None,
                RaceObjectPhase::Hit
            ),
            case!(
                "GopTimeMine",
                TIME_MINE,
                0x7000_0107,
                3,
                33,
                16,
                Some(20),
                None,
                Some(24),
                Some((28, 1)),
                Some(1),
                ItemLifecycleMeaning::Resolve,
                None,
                RaceObjectPhase::Hit
            ),
            case!(
                "GopTimeMine",
                TIME_MINE,
                0x7000_0107,
                4,
                33,
                16,
                None,
                None,
                None,
                Some((28, 1)),
                None,
                ItemLifecycleMeaning::NoClientAction,
                None,
                RaceObjectPhase::Hit
            ),
            case!(
                "GopTimeMine",
                TIME_MINE,
                0x7000_0107,
                5,
                24,
                16,
                Some(20),
                None,
                None,
                None,
                None,
                ItemLifecycleMeaning::Resolve,
                None,
                RaceObjectPhase::Hit
            ),
        ];
        assert_eq!(cases.len(), 29);

        let mut registry = RaceObjectRegistry::default();
        for (index, case) in cases.into_iter().enumerate() {
            let sequence = u32::try_from(index).unwrap();
            let token = 0x7100_0000 | sequence;
            let source = 0x7200_0000 | sequence;
            let target = 0x7300_0000 | sequence;
            let mut raw = vec![0_u8; case.length];
            put_u32(&mut raw, 0, case.pair.0);
            put_u32(&mut raw, 4, case.pair.1);
            put_u32(&mut raw, 8, case.object_id);
            put_u32(&mut raw, case.state_offset, case.state);
            if let Some(offset) = case.token_offset {
                put_u32(&mut raw, offset, token);
            }
            if let Some(offset) = case.source_offset {
                put_u32(&mut raw, offset, source);
            }
            if let Some(offset) = case.target_offset {
                put_u32(&mut raw, offset, target);
            }
            if let Some((offset, value)) = case.variant {
                raw[offset] = value;
            }

            let mut wire = vec![0_u8; 20 + raw.len()];
            put_u32(&mut wire, 0, GAME_SLOT_PACKET_HASH);
            put_u32(&mut wire, 8, 2);
            wire[12] = 12;
            put_u32(&mut wire, 16, u32::try_from(raw.len()).unwrap());
            wire[20..].copy_from_slice(&raw);

            let parsed = parse_game_slot_packet(&wire).unwrap();
            let GameSlotBody::ItemOperation(item) = parsed.body() else {
                panic!("{} state {} missed strict parsing", case.name, case.state);
            };
            assert_eq!(item.schema.class_name, case.name);
            assert_eq!(item.state, case.state);
            assert_eq!(item.semantics.meaning, case.meaning);
            assert_eq!(item.semantics.native_phase, case.native_phase);
            assert_eq!(
                item.semantics.transition_token,
                case.token_offset.map(|_| token)
            );
            assert_eq!(
                item.semantics.source_object_id,
                case.source_offset.map(|source_offset| {
                    if Some(source_offset) == case.target_offset {
                        target
                    } else {
                        source
                    }
                })
            );
            assert_eq!(
                item.semantics.target_object_id,
                case.target_offset.map(|_| target)
            );
            assert_eq!(item.semantics.variant, case.expected_variant);

            let admission = registry
                .plan(RaceObjectOperation {
                    race_epoch: 700,
                    class: RaceObjectClass {
                        operation_hash: item.operation_hash,
                        base_hash: item.operation_base_hash,
                    },
                    class_name: item.schema.class_name,
                    object_id: item.object_id,
                    state: item.state,
                    meaning: item.semantics.meaning,
                    evidence: item.semantics.evidence,
                    transition_token: item.semantics.transition_token,
                    source_object_id: item.semantics.source_object_id,
                    target_object_id: item.semantics.target_object_id,
                    variant: item.semantics.variant,
                    reporter: OWNER,
                    owner_claim: matches!(
                        item.semantics.meaning,
                        ItemLifecycleMeaning::Initialize
                            | ItemLifecycleMeaning::Place
                            | ItemLifecycleMeaning::Launch
                            | ItemLifecycleMeaning::Activate
                    )
                    .then_some(OWNER),
                })
                .unwrap();
            if case.meaning == ItemLifecycleMeaning::NoClientAction {
                assert_eq!(admission, RaceObjectAdmission::PublishUntracked);
            } else {
                let RaceObjectAdmission::PublishTracked(mutation) = admission else {
                    panic!("{} state {} was not tracked", case.name, case.state);
                };
                assert_eq!(mutation.phase(), case.registry_phase);
                registry.commit(mutation).unwrap();
            }
            assert_eq!(
                registry.phase(700, case.object_id),
                Some(case.registry_phase)
            );
        }
        assert_eq!(registry.len(), 7);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the literal third-pass table crosses every recovered state through registry admission"
    )]
    fn third_pass_literal_states_cross_wire_semantics_and_registry() {
        #[derive(Clone, Copy)]
        struct Case {
            name: &'static str,
            pair: (u32, u32),
            object_id: u32,
            state: u32,
            length: usize,
            meaning: ItemLifecycleMeaning,
            native_phase: Option<u8>,
            registry_phase: RaceObjectPhase,
        }

        fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }

        const AREA: (u32, u32) = (0x1457_03C9, 0x2199_04E8);
        const LOCKDOWN: (u32, u32) = (0x3BEA_06CF, 0x5105_07EE);
        const MOVING: (u32, u32) = (0x1E52_04C0, 0x2DD2_05DF);
        const SHIELD: (u32, u32) = (0x1110_037F, 0x1D33_049E);
        const SPECIAL: (u32, u32) = (0x3473_0640, 0x486F_075F);
        const THUNDER: (u32, u32) = (0x2973_05B1, 0x3B31_06D0);
        const UFO: (u32, u32) = (0x07CF_0250, 0x1095_036F);

        let cases = [
            Case {
                name: "GopAreaUfo",
                pair: AREA,
                object_id: 0x9000_0001,
                state: 1,
                length: 33,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
            },
            Case {
                name: "GopAreaUfo",
                pair: AREA,
                object_id: 0x9000_0001,
                state: 2,
                length: 24,
                meaning: ItemLifecycleMeaning::Resolve,
                native_phase: Some(5),
                registry_phase: RaceObjectPhase::Active,
            },
            Case {
                name: "GopMovingUfo",
                pair: MOVING,
                object_id: 0x9000_0002,
                state: 1,
                length: 72,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
            },
            Case {
                name: "GopMovingUfo",
                pair: MOVING,
                object_id: 0x9000_0002,
                state: 2,
                length: 24,
                meaning: ItemLifecycleMeaning::Impact,
                native_phase: None,
                registry_phase: RaceObjectPhase::Hit,
            },
            Case {
                name: "GopShield",
                pair: SHIELD,
                object_id: 0x9000_0003,
                state: 1,
                length: 31,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
            },
            Case {
                name: "GopShield",
                pair: SHIELD,
                object_id: 0x9000_0013,
                state: 2,
                length: 29,
                meaning: ItemLifecycleMeaning::Impact,
                native_phase: Some(1),
                registry_phase: RaceObjectPhase::Hit,
            },
            Case {
                name: "GopSpecialShield",
                pair: SPECIAL,
                object_id: 0x9000_0004,
                state: 0,
                length: 27,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
            },
            Case {
                name: "GopSpecialShield",
                pair: SPECIAL,
                object_id: 0x9000_0004,
                state: 2,
                length: 25,
                meaning: ItemLifecycleMeaning::Impact,
                native_phase: Some(2),
                registry_phase: RaceObjectPhase::Hit,
            },
            Case {
                name: "GopSpecialShield",
                pair: SPECIAL,
                object_id: 0x9000_0004,
                state: 3,
                length: 25,
                meaning: ItemLifecycleMeaning::Resolve,
                native_phase: Some(3),
                registry_phase: RaceObjectPhase::Hit,
            },
            Case {
                name: "GopUfo",
                pair: UFO,
                object_id: 0x9000_0005,
                state: 1,
                length: 33,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
            },
            Case {
                name: "GopUfo",
                pair: UFO,
                object_id: 0x9000_0005,
                state: 2,
                length: 20,
                meaning: ItemLifecycleMeaning::Resolve,
                native_phase: None,
                registry_phase: RaceObjectPhase::Active,
            },
            Case {
                name: "GopThunderbolt",
                pair: THUNDER,
                object_id: 0x9000_0007,
                state: 1,
                length: 42,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
            },
            Case {
                name: "GopThunderbolt",
                pair: THUNDER,
                object_id: 0x9000_0007,
                state: 2,
                length: 25,
                meaning: ItemLifecycleMeaning::Impact,
                native_phase: Some(4),
                registry_phase: RaceObjectPhase::Hit,
            },
            Case {
                name: "GopThunderbolt",
                pair: THUNDER,
                object_id: 0x9000_0007,
                state: 3,
                length: 29,
                meaning: ItemLifecycleMeaning::Impact,
                native_phase: Some(3),
                registry_phase: RaceObjectPhase::Hit,
            },
            Case {
                name: "GopLockdownRocket",
                pair: LOCKDOWN,
                object_id: 0x9000_0006,
                state: 1,
                length: 20,
                meaning: ItemLifecycleMeaning::Launch,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
            },
            Case {
                name: "GopLockdownRocket",
                pair: LOCKDOWN,
                object_id: 0x9000_0006,
                state: 2,
                length: 17,
                meaning: ItemLifecycleMeaning::Retarget,
                native_phase: None,
                registry_phase: RaceObjectPhase::Active,
            },
            Case {
                name: "GopLockdownRocket",
                pair: LOCKDOWN,
                object_id: 0x9000_0006,
                state: 4,
                length: 25,
                meaning: ItemLifecycleMeaning::Impact,
                native_phase: Some(1),
                registry_phase: RaceObjectPhase::Hit,
            },
            Case {
                name: "GopLockdownRocket",
                pair: LOCKDOWN,
                object_id: 0x9000_0006,
                state: 5,
                length: 18,
                meaning: ItemLifecycleMeaning::Resolve,
                native_phase: None,
                registry_phase: RaceObjectPhase::Hit,
            },
            Case {
                name: "GopLockdownRocket",
                pair: LOCKDOWN,
                object_id: 0x9000_0006,
                state: 6,
                length: 18,
                meaning: ItemLifecycleMeaning::Resolve,
                native_phase: None,
                registry_phase: RaceObjectPhase::Hit,
            },
            Case {
                name: "GopLockdownRocket",
                pair: LOCKDOWN,
                object_id: 0x9000_0006,
                state: 7,
                length: 26,
                meaning: ItemLifecycleMeaning::Resolve,
                native_phase: Some(8),
                registry_phase: RaceObjectPhase::Hit,
            },
            Case {
                name: "GopLockdownRocket",
                pair: LOCKDOWN,
                object_id: 0x9000_0006,
                state: 8,
                length: 26,
                meaning: ItemLifecycleMeaning::Resolve,
                native_phase: Some(10),
                registry_phase: RaceObjectPhase::Hit,
            },
            Case {
                name: "GopLockdownRocket",
                pair: LOCKDOWN,
                object_id: 0x9000_0006,
                state: 9,
                length: 25,
                meaning: ItemLifecycleMeaning::Resolve,
                native_phase: Some(9),
                registry_phase: RaceObjectPhase::Hit,
            },
            Case {
                name: "GopLockdownRocket",
                pair: LOCKDOWN,
                object_id: 0x9000_0006,
                state: 3,
                length: 13,
                meaning: ItemLifecycleMeaning::Remove,
                native_phase: None,
                registry_phase: RaceObjectPhase::Removed,
            },
        ];
        assert_eq!(cases.len(), 23);

        let mut registry = RaceObjectRegistry::default();
        for (index, case) in cases.into_iter().enumerate() {
            let sequence = u32::try_from(index).unwrap();
            let token = 0x9100_0000 | sequence;
            let source = 0x9200_0000 | sequence;
            let target = 0x9300_0000 | sequence;
            let mut raw = vec![0_u8; case.length];
            put_u32(&mut raw, 0, case.pair.0);
            put_u32(&mut raw, 4, case.pair.1);
            put_u32(&mut raw, 8, case.object_id);
            if case.pair == LOCKDOWN {
                raw[12] = u8::try_from(case.state).unwrap();
            } else {
                put_u32(&mut raw, 12, case.state);
            }

            match (case.pair, case.state) {
                (AREA | UFO, 1) => {
                    put_u32(&mut raw, 16, token);
                    raw[20] = 1;
                    put_u32(&mut raw, 21, target);
                    put_u32(&mut raw, 25, source);
                    put_u32(&mut raw, 29, sequence);
                }
                (AREA | MOVING, 2) => {
                    put_u32(&mut raw, 16, token);
                    put_u32(&mut raw, 20, target);
                }
                (MOVING, 1) => {
                    put_u32(&mut raw, 16, token);
                    put_u32(&mut raw, 20, source);
                }
                (SHIELD, 1) => {
                    put_u32(&mut raw, 16, token);
                    put_u32(&mut raw, 22, source);
                    raw[30] = 1;
                }
                (SHIELD, 2) => {
                    put_u32(&mut raw, 16, token);
                    put_u32(&mut raw, 20, source);
                    put_u32(&mut raw, 24, target);
                    raw[28] = 1;
                }
                (SPECIAL, 0) => {
                    raw[16] = 1;
                    put_u32(&mut raw, 17, token);
                    put_u32(&mut raw, 22, source);
                }
                (SPECIAL, 2 | 3) => {
                    raw[16] = 1;
                    put_u32(&mut raw, 17, token);
                    put_u32(&mut raw, 21, source);
                }
                (UFO, 2) => put_u32(&mut raw, 16, token),
                (THUNDER, 1) => {
                    put_u32(&mut raw, 16, token);
                    raw[20] = 1;
                    put_u32(&mut raw, 21, source);
                    put_u32(&mut raw, 25, 3);
                    for target_index in 0..3 {
                        put_u32(
                            &mut raw,
                            29 + target_index * 4,
                            target + u32::try_from(target_index).unwrap(),
                        );
                    }
                    raw[41] = 2;
                }
                (THUNDER, 2) => {
                    raw[16] = 1;
                    put_u32(&mut raw, 17, token);
                    put_u32(&mut raw, 21, target);
                }
                (THUNDER, 3) => {
                    raw[16] = 1;
                    put_u32(&mut raw, 17, token);
                    put_u32(&mut raw, 21, target);
                    put_u32(&mut raw, 25, source);
                }
                (LOCKDOWN, 1) => {
                    raw[13] = 1;
                    put_u32(&mut raw, 14, token);
                }
                (LOCKDOWN, 2) => put_u32(&mut raw, 13, target),
                (LOCKDOWN, 4 | 9) => {
                    put_u32(&mut raw, 13, token);
                    put_u32(&mut raw, 17, source);
                    put_u32(&mut raw, 21, target);
                }
                (LOCKDOWN, 5 | 6) => {
                    put_u32(&mut raw, 13, token);
                    raw[17] = 1;
                }
                (LOCKDOWN, 7 | 8) => {
                    put_u32(&mut raw, 13, token);
                    put_u32(&mut raw, 17, source);
                    put_u32(&mut raw, 21, target);
                    raw[25] = 1;
                }
                (LOCKDOWN, 3) => {}
                _ => unreachable!("literal case table and field writer drifted"),
            }

            let mut wire = vec![0_u8; 20 + raw.len()];
            put_u32(&mut wire, 0, GAME_SLOT_PACKET_HASH);
            put_u32(&mut wire, 8, 2);
            wire[12] = 12;
            put_u32(&mut wire, 16, u32::try_from(raw.len()).unwrap());
            wire[20..].copy_from_slice(&raw);

            let parsed = parse_game_slot_packet(&wire).unwrap();
            let GameSlotBody::ItemOperation(item) = parsed.body() else {
                panic!("{} state {} missed strict parsing", case.name, case.state);
            };
            assert_eq!(item.schema.class_name, case.name);
            assert_eq!(item.semantics.meaning, case.meaning);
            assert_eq!(item.semantics.native_phase, case.native_phase);
            if case.pair == THUNDER && case.state == 1 {
                assert_eq!(
                    item.semantics
                        .target_object_ids
                        .expect("Thunderbolt state 1 exposes its counted target set")
                        .decode(&raw)
                        .unwrap(),
                    [target, target + 1, target + 2]
                );
            } else {
                assert_eq!(item.semantics.target_object_ids, None);
            }

            let admission = registry
                .plan(RaceObjectOperation {
                    race_epoch: 800,
                    class: RaceObjectClass {
                        operation_hash: item.operation_hash,
                        base_hash: item.operation_base_hash,
                    },
                    class_name: item.schema.class_name,
                    object_id: item.object_id,
                    state: item.state,
                    meaning: item.semantics.meaning,
                    evidence: item.semantics.evidence,
                    transition_token: item.semantics.transition_token,
                    source_object_id: item.semantics.source_object_id,
                    target_object_id: item.semantics.target_object_id,
                    variant: item.semantics.variant,
                    reporter: OWNER,
                    owner_claim: matches!(
                        item.semantics.meaning,
                        ItemLifecycleMeaning::Launch | ItemLifecycleMeaning::Activate
                    )
                    .then_some(OWNER),
                })
                .unwrap();
            let RaceObjectAdmission::PublishTracked(mutation) = admission else {
                panic!("{} state {} was not tracked", case.name, case.state);
            };
            assert_eq!(mutation.phase(), case.registry_phase);
            registry.commit(mutation).unwrap();
            assert_eq!(
                registry.phase(800, case.object_id),
                Some(case.registry_phase)
            );
        }
        assert_eq!(
            registry.phase(800, 0x9000_0003),
            Some(RaceObjectPhase::Active)
        );
        assert_eq!(registry.phase(800, 0x9000_0013), Some(RaceObjectPhase::Hit));
        assert_eq!(registry.len(), 8);
    }

    #[test]
    #[allow(
        clippy::match_same_arms,
        clippy::too_many_lines,
        reason = "class-local literal arms keep each recovered wire binding auditable through registry admission"
    )]
    fn ordinary_effect_states_cross_wire_semantics_and_registry() {
        #[derive(Clone, Copy)]
        struct Case {
            name: &'static str,
            pair: (u32, u32),
            object_id: u32,
            state: u32,
            length: usize,
            meaning: ItemLifecycleMeaning,
            native_phase: Option<u8>,
            registry_phase: RaceObjectPhase,
            tracked: bool,
        }

        fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }

        const FORCE: (u32, u32) = (0x1DC6_04B1, 0x2D46_05D0);
        const OIL: (u32, u32) = (0x07C0_024A, 0x1086_0369);
        const SILENCE: (u32, u32) = (0x150D_03E9, 0x224F_0508);
        const SIREN: (u32, u32) = (0x0DB2_0327, 0x18B6_0446);
        const SIREN_SHIELD: (u32, u32) = (0x28A5_0580, 0x3A63_069F);
        const SMALL: (u32, u32) = (0x2E3D_05E0, 0x411A_06FF);
        const CLOUD: (u32, u32) = (0x0D7B_031D, 0x187F_043C);
        const CLOUD2: (u32, u32) = (0x10CA_034F, 0x1CED_046E);
        const MAGNET: (u32, u32) = (0x10DE_0382, 0x1D01_04A1);
        const SPEED_DOWN: (u32, u32) = (0x1DB2_04AF, 0x2D32_05CE);
        const DEVIL: (u32, u32) = (0x0D69_031A, 0x186D_0439);
        const MQ_DEVIL: (u32, u32) = (0x1476_03D8, 0x21B8_04F7);
        const NEW_DEVIL: (u32, u32) = (0x18D8_0444, 0x2739_0563);

        let cases = [
            Case {
                name: "GopForceZone",
                pair: FORCE,
                object_id: 0xA000_0001,
                state: 1,
                length: 72,
                meaning: ItemLifecycleMeaning::Place,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
                tracked: true,
            },
            Case {
                name: "GopForceZone",
                pair: FORCE,
                object_id: 0xA000_0001,
                state: 2,
                length: 29,
                meaning: ItemLifecycleMeaning::Impact,
                native_phase: Some(2),
                registry_phase: RaceObjectPhase::Hit,
                tracked: true,
            },
            Case {
                name: "GopForceZone",
                pair: FORCE,
                object_id: 0xA000_0001,
                state: 3,
                length: 29,
                meaning: ItemLifecycleMeaning::Resolve,
                native_phase: Some(3),
                registry_phase: RaceObjectPhase::Hit,
                tracked: true,
            },
            Case {
                name: "GopForceZone",
                pair: FORCE,
                object_id: 0xA000_0001,
                state: 5,
                length: 25,
                meaning: ItemLifecycleMeaning::Resolve,
                native_phase: Some(5),
                registry_phase: RaceObjectPhase::Hit,
                tracked: true,
            },
            Case {
                name: "GopOil",
                pair: OIL,
                object_id: 0xA000_0002,
                state: 1,
                length: 73,
                meaning: ItemLifecycleMeaning::Place,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
                tracked: true,
            },
            Case {
                name: "GopOil",
                pair: OIL,
                object_id: 0xA000_0002,
                state: 2,
                length: 29,
                meaning: ItemLifecycleMeaning::Impact,
                native_phase: Some(2),
                registry_phase: RaceObjectPhase::Hit,
                tracked: true,
            },
            Case {
                name: "GopOil",
                pair: OIL,
                object_id: 0xA000_0002,
                state: 3,
                length: 25,
                meaning: ItemLifecycleMeaning::Resolve,
                native_phase: Some(3),
                registry_phase: RaceObjectPhase::Hit,
                tracked: true,
            },
            Case {
                name: "GopSilence",
                pair: SILENCE,
                object_id: 0xA000_0003,
                state: 1,
                length: 29,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
                tracked: true,
            },
            Case {
                name: "GopSilence",
                pair: SILENCE,
                object_id: 0xA000_0003,
                state: 2,
                length: 29,
                meaning: ItemLifecycleMeaning::NoClientAction,
                native_phase: None,
                registry_phase: RaceObjectPhase::Active,
                tracked: false,
            },
            Case {
                name: "GopSiren",
                pair: SIREN,
                object_id: 0xA000_0004,
                state: 1,
                length: 26,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
                tracked: true,
            },
            Case {
                name: "GopSiren",
                pair: SIREN,
                object_id: 0xA000_0004,
                state: 2,
                length: 31,
                meaning: ItemLifecycleMeaning::Impact,
                native_phase: Some(1),
                registry_phase: RaceObjectPhase::Hit,
                tracked: true,
            },
            Case {
                name: "GopSirenShield",
                pair: SIREN_SHIELD,
                object_id: 0xA000_0005,
                state: 0,
                length: 25,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
                tracked: true,
            },
            Case {
                name: "GopSirenShield",
                pair: SIREN_SHIELD,
                object_id: 0xA000_0005,
                state: 1,
                length: 24,
                meaning: ItemLifecycleMeaning::Impact,
                native_phase: Some(1),
                registry_phase: RaceObjectPhase::Hit,
                tracked: true,
            },
            Case {
                name: "GopSirenShield",
                pair: SIREN_SHIELD,
                object_id: 0xA000_0005,
                state: 2,
                length: 25,
                meaning: ItemLifecycleMeaning::Resolve,
                native_phase: Some(2),
                registry_phase: RaceObjectPhase::Hit,
                tracked: true,
            },
            Case {
                name: "GopSpecialSmall",
                pair: SMALL,
                object_id: 0xA000_0006,
                state: 0,
                length: 30,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
                tracked: true,
            },
            Case {
                name: "GopSpecialSmall",
                pair: SMALL,
                object_id: 0xA000_0006,
                state: 1,
                length: 29,
                meaning: ItemLifecycleMeaning::Impact,
                native_phase: Some(3),
                registry_phase: RaceObjectPhase::Hit,
                tracked: true,
            },
            Case {
                name: "GopSpecialSmall",
                pair: SMALL,
                object_id: 0xA000_0006,
                state: 2,
                length: 17,
                meaning: ItemLifecycleMeaning::UpdateRuntimeFlag,
                native_phase: None,
                registry_phase: RaceObjectPhase::Hit,
                tracked: true,
            },
            Case {
                name: "GopCloud",
                pair: CLOUD,
                object_id: 0xA000_0007,
                state: 1,
                length: 73,
                meaning: ItemLifecycleMeaning::Place,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
                tracked: true,
            },
            Case {
                name: "GopCloud",
                pair: CLOUD,
                object_id: 0xA000_0007,
                state: 2,
                length: 20,
                meaning: ItemLifecycleMeaning::Impact,
                native_phase: Some(2),
                registry_phase: RaceObjectPhase::Hit,
                tracked: true,
            },
            Case {
                name: "GopCloud2",
                pair: CLOUD2,
                object_id: 0xA000_0008,
                state: 1,
                length: 73,
                meaning: ItemLifecycleMeaning::Place,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
                tracked: true,
            },
            Case {
                name: "GopCloud2",
                pair: CLOUD2,
                object_id: 0xA000_0008,
                state: 2,
                length: 20,
                meaning: ItemLifecycleMeaning::Impact,
                native_phase: Some(2),
                registry_phase: RaceObjectPhase::Hit,
                tracked: true,
            },
            Case {
                name: "GopMagnet",
                pair: MAGNET,
                object_id: 0xA000_0009,
                state: 1,
                length: 30,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(1),
                registry_phase: RaceObjectPhase::Active,
                tracked: true,
            },
            Case {
                name: "GopSpeedDown",
                pair: SPEED_DOWN,
                object_id: 0xA000_0010,
                state: 1,
                length: 24,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
                tracked: true,
            },
            Case {
                name: "GopSpeedDown",
                pair: SPEED_DOWN,
                object_id: 0xA000_0010,
                state: 2,
                length: 20,
                meaning: ItemLifecycleMeaning::Remove,
                native_phase: Some(2),
                registry_phase: RaceObjectPhase::Removed,
                tracked: true,
            },
            Case {
                name: "GopDevil",
                pair: DEVIL,
                object_id: 0xA000_0011,
                state: 1,
                length: 31,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
                tracked: true,
            },
            Case {
                name: "GopMqDevil",
                pair: MQ_DEVIL,
                object_id: 0xA000_0012,
                state: 1,
                length: 31,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
                tracked: true,
            },
            Case {
                name: "GopNewDevil",
                pair: NEW_DEVIL,
                object_id: 0xA000_0013,
                state: 1,
                length: 27,
                meaning: ItemLifecycleMeaning::Activate,
                native_phase: Some(0),
                registry_phase: RaceObjectPhase::Active,
                tracked: true,
            },
        ];
        assert_eq!(cases.len(), 27);

        let mut registry = RaceObjectRegistry::default();
        let mut special_small_impact = None;
        let mut cloud_impact = None;
        for (index, case) in cases.into_iter().enumerate() {
            let sequence = u32::try_from(index).unwrap();
            let token = 0xA100_0000 | sequence;
            let source = 0xA200_0000 | sequence;
            let target = 0xA300_0000 | sequence;
            let mut raw = vec![0_u8; case.length];
            put_u32(&mut raw, 0, case.pair.0);
            put_u32(&mut raw, 4, case.pair.1);
            put_u32(&mut raw, 8, case.object_id);
            put_u32(&mut raw, 12, case.state);

            match (case.pair, case.state) {
                (FORCE, 1) => {
                    put_u32(&mut raw, 16, token);
                    put_u32(&mut raw, 68, source);
                }
                (OIL, 1) => {
                    put_u32(&mut raw, 16, token);
                    raw[20] = 1;
                    put_u32(&mut raw, 69, source);
                }
                (FORCE | OIL, 2) => {
                    put_u32(&mut raw, 16, token);
                    put_u32(&mut raw, 20, target);
                    raw[24] = 1;
                    put_u32(&mut raw, 25, source);
                }
                (FORCE, 3) => {
                    put_u32(&mut raw, 16, token);
                    put_u32(&mut raw, 20, source);
                    raw[24] = 1;
                    put_u32(&mut raw, 25, target);
                }
                (OIL, 3) | (FORCE, 5) => {
                    put_u32(&mut raw, 16, token);
                    put_u32(&mut raw, 20, source);
                    raw[24] = 1;
                }
                (SILENCE, 1 | 2) => {
                    raw[16] = 1;
                    put_u32(&mut raw, 17, token);
                    put_u32(&mut raw, 21, source);
                    put_u32(&mut raw, 25, target);
                }
                (SIREN, 1) => {
                    put_u32(&mut raw, 16, token);
                    raw[20] = 1;
                    put_u32(&mut raw, 21, source);
                    raw[25] = 2;
                }
                (SIREN, 2) => {
                    put_u32(&mut raw, 16, token);
                    put_u32(&mut raw, 20, target);
                    put_u32(&mut raw, 24, source);
                    raw[28] = 1;
                }
                (SIREN_SHIELD, 0 | 2) => {
                    put_u32(&mut raw, 16, token);
                    raw[20] = 1;
                    put_u32(&mut raw, 21, source);
                }
                (SIREN_SHIELD, 1) => {
                    put_u32(&mut raw, 16, token);
                    put_u32(&mut raw, 20, target);
                }
                (SMALL, 0 | 1) => {
                    put_u32(&mut raw, 16, token);
                    put_u32(&mut raw, 20, target);
                    put_u32(&mut raw, 24, source);
                    raw[case.length - 1] = 1;
                }
                (SMALL, 2) => raw[16] = 1,
                (CLOUD | CLOUD2, 1) => {
                    put_u32(&mut raw, 16, token);
                    put_u32(&mut raw, 20, source);
                    raw[24] = 1;
                }
                (CLOUD | CLOUD2, 2) => put_u32(&mut raw, 16, target),
                (MAGNET, 1) => {
                    put_u32(&mut raw, 16, token);
                    put_u32(&mut raw, 20, source);
                    put_u32(&mut raw, 24, target);
                    raw[28..30].copy_from_slice(&1_u16.to_le_bytes());
                }
                (SPEED_DOWN, 1) => {
                    put_u32(&mut raw, 16, token);
                    put_u32(&mut raw, 20, target);
                }
                (SPEED_DOWN, 2) => put_u32(&mut raw, 16, token),
                (DEVIL | MQ_DEVIL, 1) => {
                    put_u32(&mut raw, 16, token);
                    raw[20] = 5;
                    put_u32(&mut raw, 21, source);
                    raw[25] = 1;
                    raw[26] = 2;
                    put_u32(&mut raw, 27, target);
                }
                (NEW_DEVIL, 1) => {
                    put_u32(&mut raw, 16, token);
                    raw[20] = 5;
                    put_u32(&mut raw, 21, source);
                    raw[25] = 1;
                    raw[26] = 2;
                }
                _ => unreachable!("literal case table and field writer drifted"),
            }

            let mut wire = vec![0_u8; 20 + raw.len()];
            put_u32(&mut wire, 0, GAME_SLOT_PACKET_HASH);
            put_u32(&mut wire, 8, 2);
            wire[12] = 12;
            put_u32(&mut wire, 16, u32::try_from(raw.len()).unwrap());
            wire[20..].copy_from_slice(&raw);

            let parsed = parse_game_slot_packet(&wire).unwrap();
            let GameSlotBody::ItemOperation(item) = parsed.body() else {
                panic!("{} state {} missed strict parsing", case.name, case.state);
            };
            assert_eq!(item.schema.class_name, case.name);
            assert_eq!(item.semantics.meaning, case.meaning);
            assert_eq!(item.semantics.native_phase, case.native_phase);

            let registry_operation = RaceObjectOperation {
                race_epoch: 900,
                class: RaceObjectClass {
                    operation_hash: item.operation_hash,
                    base_hash: item.operation_base_hash,
                },
                class_name: item.schema.class_name,
                object_id: item.object_id,
                state: item.state,
                meaning: item.semantics.meaning,
                evidence: item.semantics.evidence,
                transition_token: item.semantics.transition_token,
                source_object_id: item.semantics.source_object_id,
                target_object_id: item.semantics.target_object_id,
                variant: item.semantics.variant,
                reporter: OWNER,
                owner_claim: matches!(
                    item.semantics.meaning,
                    ItemLifecycleMeaning::Place | ItemLifecycleMeaning::Activate
                )
                .then_some(OWNER),
            };
            if case.pair == SMALL && case.state == 1 {
                special_small_impact = Some(registry_operation);
            }
            if case.pair == CLOUD && case.state == 2 {
                cloud_impact = Some(registry_operation);
            }
            let admission = registry.plan(registry_operation).unwrap();
            if case.tracked {
                let RaceObjectAdmission::PublishTracked(mutation) = admission else {
                    panic!("{} state {} was not tracked", case.name, case.state);
                };
                assert_eq!(mutation.phase(), case.registry_phase);
                if case.pair == SMALL && case.state == 2 {
                    assert_eq!(mutation.runtime_flag(), Some(1));
                    assert_eq!(mutation.state(), 1);
                    assert_eq!(mutation.meaning(), ItemLifecycleMeaning::Impact);
                }
                registry.commit(mutation).unwrap();
            } else {
                assert_eq!(admission, RaceObjectAdmission::PublishUntracked);
            }
            assert_eq!(
                registry.phase(900, case.object_id),
                Some(case.registry_phase)
            );
        }
        assert_eq!(registry.len(), 13);
        assert!(matches!(
            registry
                .plan(special_small_impact.expect("SpecialSmall impact fixture was visited"))
                .unwrap(),
            RaceObjectAdmission::SuppressDuplicate {
                kind: RaceObjectDuplicateKind::Hit,
                ..
            }
        ));

        let cloud_impact = cloud_impact.expect("Cloud impact fixture was visited");
        assert!(matches!(
            registry.plan(cloud_impact).unwrap(),
            RaceObjectAdmission::SuppressDuplicate {
                kind: RaceObjectDuplicateKind::Hit,
                ..
            }
        ));
        let next_target = cloud_impact
            .target_object_id
            .expect("Cloud state 2 binds a target")
            + 1;
        let distinct_target = RaceObjectOperation {
            target_object_id: Some(next_target),
            ..cloud_impact
        };
        let RaceObjectAdmission::PublishTracked(mutation) = registry.plan(distinct_target).unwrap()
        else {
            panic!("a different Cloud target must remain publishable");
        };
        registry.commit(mutation).unwrap();
        assert!(matches!(
            registry.plan(distinct_target).unwrap(),
            RaceObjectAdmission::SuppressDuplicate {
                kind: RaceObjectDuplicateKind::Hit,
                ..
            }
        ));
    }

    #[test]
    #[allow(
        clippy::match_same_arms,
        clippy::too_many_lines,
        reason = "class-local literal paths prove the distinct zero-result semantics"
    )]
    fn force_zone_and_oil_zero_results_have_distinct_terminal_semantics() {
        fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }

        fn parsed_item(
            pair: (u32, u32),
            object_id: u32,
            state: u32,
            length: usize,
        ) -> p5136_core::game_slot_protocol::ItemOperation {
            let mut raw = vec![0_u8; length];
            put_u32(&mut raw, 0, pair.0);
            put_u32(&mut raw, 4, pair.1);
            put_u32(&mut raw, 8, object_id);
            put_u32(&mut raw, 12, state);
            let mut wire = vec![0_u8; 20 + length];
            put_u32(&mut wire, 0, GAME_SLOT_PACKET_HASH);
            put_u32(&mut wire, 8, 2);
            wire[12] = 12;
            put_u32(&mut wire, 16, u32::try_from(length).unwrap());
            wire[20..].copy_from_slice(&raw);
            let parsed = parse_game_slot_packet(&wire).unwrap();
            let GameSlotBody::ItemOperation(item) = parsed.body() else {
                panic!("ordinary-effect fixture missed strict parsing");
            };
            *item
        }

        let cases = [
            (
                (0x1DC6_04B1, 0x2D46_05D0),
                0xB000_0001,
                72,
                ItemLifecycleMeaning::Resolve,
                RaceObjectPhase::Active,
            ),
            (
                (0x07C0_024A, 0x1086_0369),
                0xB000_0011,
                73,
                ItemLifecycleMeaning::Remove,
                RaceObjectPhase::Removed,
            ),
        ];
        let mut registry = RaceObjectRegistry::default();
        for (pair, base_object_id, install_length, failure_meaning, failure_phase) in cases {
            for failure_state in [2, 3] {
                let object_id = base_object_id + failure_state - 2;
                let install = parsed_item(pair, object_id, 1, install_length);
                let install_operation = RaceObjectOperation {
                    race_epoch: 901,
                    class: RaceObjectClass {
                        operation_hash: install.operation_hash,
                        base_hash: install.operation_base_hash,
                    },
                    class_name: install.schema.class_name,
                    object_id: install.object_id,
                    state: install.state,
                    meaning: install.semantics.meaning,
                    evidence: install.semantics.evidence,
                    transition_token: install.semantics.transition_token,
                    source_object_id: install.semantics.source_object_id,
                    target_object_id: install.semantics.target_object_id,
                    variant: install.semantics.variant,
                    reporter: OWNER,
                    owner_claim: Some(OWNER),
                };
                let RaceObjectAdmission::PublishTracked(install_mutation) =
                    registry.plan(install_operation).unwrap()
                else {
                    panic!("placement must be tracked");
                };
                registry.commit(install_mutation).unwrap();

                // raw success@24 remains zero in this literal fixture.
                let failure_length = match (pair, failure_state) {
                    ((0x1DC6_04B1, 0x2D46_05D0), 2 | 3) => 29,
                    ((0x07C0_024A, 0x1086_0369), 2) => 29,
                    ((0x07C0_024A, 0x1086_0369), 3) => 25,
                    _ => unreachable!("literal failure case drifted"),
                };
                let failure = parsed_item(pair, object_id, failure_state, failure_length);
                assert_eq!(failure.semantics.meaning, failure_meaning);
                assert_eq!(failure.semantics.native_phase, None);
                let failure_operation = RaceObjectOperation {
                    race_epoch: 901,
                    class: install_operation.class,
                    class_name: failure.schema.class_name,
                    object_id: failure.object_id,
                    state: failure.state,
                    meaning: failure.semantics.meaning,
                    evidence: failure.semantics.evidence,
                    transition_token: failure.semantics.transition_token,
                    source_object_id: failure.semantics.source_object_id,
                    target_object_id: failure.semantics.target_object_id,
                    variant: failure.semantics.variant,
                    reporter: OWNER,
                    owner_claim: None,
                };
                let RaceObjectAdmission::PublishTracked(failure_mutation) =
                    registry.plan(failure_operation).unwrap()
                else {
                    panic!("known zero-result transition must remain tracked");
                };
                assert_eq!(failure_mutation.phase(), failure_phase);
                registry.commit(failure_mutation).unwrap();
            }
        }
        for object_id in [0xB000_0001, 0xB000_0002] {
            assert_eq!(
                registry.phase(901, object_id),
                Some(RaceObjectPhase::Active)
            );
        }
        for object_id in [0xB000_0011, 0xB000_0012] {
            assert_eq!(
                registry.phase(901, object_id),
                Some(RaceObjectPhase::Removed)
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the literal integration sequence keeps registry effects adjacent to wire evidence"
    )]
    fn fourth_pass_known_states_track_and_no_action_states_stay_untracked() {
        fn parsed_item(
            pair: (u32, u32),
            object_id: u32,
            state: u32,
            length: usize,
        ) -> p5136_core::game_slot_protocol::ItemOperation {
            let mut raw = vec![0_u8; length];
            raw[0..4].copy_from_slice(&pair.0.to_le_bytes());
            raw[4..8].copy_from_slice(&pair.1.to_le_bytes());
            raw[8..12].copy_from_slice(&object_id.to_le_bytes());
            raw[12..16].copy_from_slice(&state.to_le_bytes());
            let mut wire = vec![0_u8; 20 + length];
            wire[0..4].copy_from_slice(&GAME_SLOT_PACKET_HASH.to_le_bytes());
            wire[8..12].copy_from_slice(&2_u32.to_le_bytes());
            wire[12] = 12;
            wire[16..20].copy_from_slice(&u32::try_from(length).unwrap().to_le_bytes());
            wire[20..].copy_from_slice(&raw);
            let parsed = parse_game_slot_packet(&wire).unwrap();
            let GameSlotBody::ItemOperation(item) = parsed.body() else {
                panic!("fixture missed strict item parsing");
            };
            *item
        }

        fn operation(
            item: p5136_core::game_slot_protocol::ItemOperation,
            owner_claim: bool,
        ) -> RaceObjectOperation {
            RaceObjectOperation {
                race_epoch: 902,
                class: RaceObjectClass {
                    operation_hash: item.operation_hash,
                    base_hash: item.operation_base_hash,
                },
                class_name: item.schema.class_name,
                object_id: item.object_id,
                state: item.state,
                meaning: item.semantics.meaning,
                evidence: item.semantics.evidence,
                transition_token: item.semantics.transition_token,
                source_object_id: item.semantics.source_object_id,
                target_object_id: item.semantics.target_object_id,
                variant: item.semantics.variant,
                reporter: OWNER,
                owner_claim: owner_claim.then_some(OWNER),
            }
        }

        let installs = [
            ((0x0D49_030D, 0x184D_042C), 0xC000_0001, 0, 25),
            ((0x07AE_0248, 0x1074_0367), 0xC000_0002, 0, 26),
            ((0x0D8B_032B, 0x188F_044A), 0xC000_0003, 1, 29),
            ((0x10C3_0382, 0x1CE6_04A1), 0xC000_0004, 1, 78),
            ((0x1942_0457, 0x27A3_0576), 0xC000_0005, 1, 30),
            ((0x196B_0451, 0x27CC_0570), 0xC000_0006, 1, 29),
            ((0x2E54_05E8, 0x4131_0707), 0xC000_0007, 0, 26),
            ((0x2262_0502, 0x3301_0621), 0xC000_0008, 0, 30),
            ((0x3C6F_06D4, 0x518A_07F3), 0xC000_0009, 1, 58),
        ];
        let mut registry = RaceObjectRegistry::default();
        for (pair, object_id, state, length) in installs {
            let item = parsed_item(pair, object_id, state, length);
            assert!(matches!(
                item.semantics.meaning,
                ItemLifecycleMeaning::Activate | ItemLifecycleMeaning::Launch
            ));
            let RaceObjectAdmission::PublishTracked(mutation) =
                registry.plan(operation(item, true)).unwrap()
            else {
                panic!("{} install was not tracked", item.schema.class_name);
            };
            assert_eq!(mutation.phase(), RaceObjectPhase::Active);
            registry.commit(mutation).unwrap();
        }

        let angel_impact = parsed_item((0x0D49_030D, 0x184D_042C), 0xC000_0011, 2, 28);
        assert_eq!(angel_impact.semantics.meaning, ItemLifecycleMeaning::Impact);
        let RaceObjectAdmission::PublishTracked(angel_hit) =
            registry.plan(operation(angel_impact, false)).unwrap()
        else {
            panic!("Angel defense impact was not tracked");
        };
        assert_eq!(angel_hit.phase(), RaceObjectPhase::Hit);
        registry.commit(angel_hit).unwrap();
        assert_eq!(
            registry.phase(902, 0xC000_0001),
            Some(RaceObjectPhase::Active)
        );
        assert_eq!(registry.phase(902, 0xC000_0011), Some(RaceObjectPhase::Hit));

        for state in [2, 3] {
            let straight_writer_only =
                parsed_item((0x3C6F_06D4, 0x518A_07F3), 0xC000_0009, state, 24);
            assert_eq!(
                straight_writer_only.semantics.meaning,
                ItemLifecycleMeaning::NoClientAction
            );
            assert_eq!(
                registry
                    .plan(operation(straight_writer_only, false))
                    .unwrap(),
                RaceObjectAdmission::PublishUntracked
            );
        }
        assert_eq!(
            registry.phase(902, 0xC000_0001),
            Some(RaceObjectPhase::Active)
        );
        assert_eq!(
            registry.phase(902, 0xC000_0009),
            Some(RaceObjectPhase::Active)
        );

        let slot_lock_hit = parsed_item((0x196B_0451, 0x27CC_0570), 0xC000_0006, 2, 29);
        let RaceObjectAdmission::PublishTracked(hit) =
            registry.plan(operation(slot_lock_hit, false)).unwrap()
        else {
            panic!("SlotLock impact was not tracked");
        };
        assert_eq!(hit.phase(), RaceObjectPhase::Hit);
        registry.commit(hit).unwrap();

        let spacecraft_hit = parsed_item((0x2262_0502, 0x3301_0621), 0xC000_0008, 2, 29);
        let RaceObjectAdmission::PublishTracked(hit) =
            registry.plan(operation(spacecraft_hit, false)).unwrap()
        else {
            panic!("SpaceCraft impact was not tracked");
        };
        registry.commit(hit).unwrap();
        let spacecraft_resolve = parsed_item((0x2262_0502, 0x3301_0621), 0xC000_0008, 4, 29);
        let RaceObjectAdmission::PublishTracked(resolve) =
            registry.plan(operation(spacecraft_resolve, false)).unwrap()
        else {
            panic!("SpaceCraft resolve was not tracked");
        };
        assert_eq!(resolve.phase(), RaceObjectPhase::Hit);
        registry.commit(resolve).unwrap();
        assert_eq!(registry.phase(902, 0xC000_0008), Some(RaceObjectPhase::Hit));
    }
}
