//! Pure, bounded, actor-owned state for P5136 `MyRoom` membership.
//!
//! The hub performs no I/O, serialization, random selection, or outbound
//! delivery. Mutations are planned as bounded deltas. The world actor may
//! inspect a transition's effects, reserve every outbound queue, and then
//! commit. Commits are revision-checked, so an old plan can never overwrite
//! newer state.

use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr},
    num::NonZeroUsize,
};

use p5136_core::{
    myroom_protocol::{
        MYROOM_SLOT_COUNT, MyRoomInfo, MyRoomPlayerSlot, MyRoomProtocolError, MyRoomSlot,
        validate_myroom_info, validate_myroom_player_slot,
    },
    nickname::canonical_nickname_key,
};
use thiserror::Error;

use crate::{IdentityBinding, ReleasedIdentity, UserNo};

#[cfg(test)]
pub(crate) const MAX_MYROOM_IDENTITIES: usize = 256;
const VISITOR_CAPACITY: usize = MYROOM_SLOT_COUNT - 1;
const MAX_TRANSITION_ROOMS: usize = 2;
const MAX_TRANSITION_MEMBERSHIPS: usize = MYROOM_SLOT_COUNT;
// An owner may visit a one-member room while owning a full room. Closing both
// can prune the owner, seven ejected visitors, and the visited-room owner.
const MAX_TRANSITION_GENERATIONS: usize = MYROOM_SLOT_COUNT + 1;

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the TCP MyRoom command tranche consumes room entry"
    )
)]
const VISITOR_SLOTS: [MyRoomSlotIndex; VISITOR_CAPACITY] = [
    MyRoomSlotIndex(1),
    MyRoomSlotIndex(2),
    MyRoomSlotIndex(3),
    MyRoomSlotIndex(4),
    MyRoomSlotIndex(5),
    MyRoomSlotIndex(6),
    MyRoomSlotIndex(7),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct OwnerKey(UserNo);

impl OwnerKey {
    const fn user_no(self) -> UserNo {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MyRoomSlotIndex(u8);

impl MyRoomSlotIndex {
    const OWNER: Self = Self(0);

    #[must_use]
    pub(crate) const fn get(self) -> u8 {
        self.0
    }

    fn visitor_index(self) -> Option<usize> {
        let index = usize::from(self.0);
        (index > 0 && index < MYROOM_SLOT_COUNT).then_some(index - 1)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(crate) struct MyRoomRevision(u64);

impl MyRoomRevision {
    #[must_use]
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MyRoomParticipant {
    identity: IdentityBinding,
    player: MyRoomPlayerSlot,
}

impl MyRoomParticipant {
    pub(crate) fn new(
        identity: IdentityBinding,
        player: MyRoomPlayerSlot,
    ) -> Result<Self, MyRoomHubError> {
        validate_myroom_player_slot(&player)?;
        if player.user_no != identity.user_no.get() {
            return Err(MyRoomHubError::SnapshotUserNoMismatch {
                identity: identity.user_no,
                snapshot: player.user_no,
            });
        }
        if canonical_nickname_key(&player.nickname) != canonical_nickname_key(&identity.nickname) {
            return Err(MyRoomHubError::SnapshotNicknameMismatch {
                identity: identity.nickname.clone(),
                snapshot: player.nickname.clone(),
            });
        }
        Ok(Self { identity, player })
    }

    #[must_use]
    #[allow(
        dead_code,
        reason = "the TCP MyRoom command boundary needs exact identity access"
    )]
    pub(crate) fn identity(&self) -> &IdentityBinding {
        &self.identity
    }

    #[must_use]
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "the TCP MyRoom command boundary needs presentation access"
        )
    )]
    pub(crate) fn player(&self) -> &MyRoomPlayerSlot {
        &self.player
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the TCP MyRoom entry command consumes owner input"
    )
)]
pub(crate) struct MyRoomOwner {
    participant: MyRoomParticipant,
    info: MyRoomInfo,
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the TCP MyRoom entry command consumes owner input"
    )
)]
impl MyRoomOwner {
    pub(crate) fn new(
        participant: MyRoomParticipant,
        info: MyRoomInfo,
    ) -> Result<Self, MyRoomHubError> {
        validate_myroom_info(&info)?;
        Ok(Self { participant, info })
    }

    fn key(&self) -> OwnerKey {
        OwnerKey(self.participant.identity.user_no)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MyRoomSnapshot {
    pub(crate) owner: UserNo,
    pub(crate) slots: [MyRoomSlot; MYROOM_SLOT_COUNT],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoomPublication {
    pub(crate) owner: UserNo,
    pub(crate) snapshot: MyRoomSnapshot,
    /// Exact canonical recipients in protocol slot order.
    pub(crate) audience: Vec<IdentityBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RoomEffect {
    Updated(Box<RoomPublication>),
    Deleted { owner: UserNo },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the TCP MyRoom entry command consumes this outcome"
    )
)]
pub(crate) enum EnterOutcome {
    Reentered {
        slot: MyRoomSlotIndex,
        publication: Box<RoomPublication>,
    },
    Moved {
        slot: MyRoomSlotIndex,
        /// Old-room work always precedes the new-room publication.
        previous: Option<RoomEffect>,
        current: Box<RoomPublication>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the TCP MyRoom leave command consumes this outcome"
    )
)]
pub(crate) struct LeaveEffects {
    pub(crate) room: RoomEffect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClosedOwnedRoom {
    pub(crate) owner: UserNo,
    pub(crate) ejected: Vec<IdentityBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisconnectEffects {
    pub(crate) membership_room: Option<RoomEffect>,
    pub(crate) closed_owned_room: Option<ClosedOwnedRoom>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MyRoomDisconnectOutcome {
    NotTracked,
    Applied(DisconnectEffects),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PeerAudience {
    pub(crate) owner: UserNo,
    pub(crate) sender_slot: MyRoomSlotIndex,
    pub(crate) peers: Vec<IdentityBinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MyRoomMembershipInfo {
    pub(crate) owner: UserNo,
    pub(crate) slot: MyRoomSlotIndex,
}

impl MyRoomMembershipInfo {
    #[must_use]
    pub(crate) fn is_present_owner(self, user_no: UserNo) -> bool {
        self.owner == user_no && self.slot == MyRoomSlotIndex::OWNER
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the TCP MyRoom info command consumes this outcome"
    )
)]
pub(crate) struct OwnerInfoUpdate {
    pub(crate) owner: UserNo,
    pub(crate) previous: MyRoomInfo,
    pub(crate) current: MyRoomInfo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the profile refresh integration consumes this outcome"
    )
)]
pub(crate) struct ParticipantRefreshEffects {
    /// Membership room first, then a distinct owned room.
    pub(crate) publications: Vec<RoomPublication>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IdentityAdvanceEffects {
    pub(crate) previous: IdentityBinding,
    pub(crate) current: IdentityBinding,
    /// Membership room first, then a distinct owned room.
    pub(crate) publications: Vec<RoomPublication>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum MyRoomCommitError {
    #[error("stale MyRoom transition planned at revision {planned}; live revision is {current}")]
    StaleRevision { planned: u64, current: u64 },
}

#[derive(Debug)]
pub(crate) struct MyRoomTransition<T> {
    base_revision: MyRoomRevision,
    next_revision: MyRoomRevision,
    rooms: Vec<RoomChange>,
    memberships: Vec<MembershipChange>,
    generations: Vec<GenerationChange>,
    outcome: T,
}

impl<T> MyRoomTransition<T> {
    #[must_use]
    pub(crate) fn outcome(&self) -> &T {
        &self.outcome
    }

    /// Applies a previously validated bounded delta. Revision validation occurs
    /// before the first mutation, so a stale commit leaves `live` untouched.
    pub(crate) fn commit(self, live: &mut MyRoomHub) -> Result<T, MyRoomCommitError> {
        if live.revision != self.base_revision {
            return Err(MyRoomCommitError::StaleRevision {
                planned: self.base_revision.get(),
                current: live.revision.get(),
            });
        }

        for change in self.rooms {
            match change.value {
                Some(room) => {
                    live.rooms.insert(change.owner, room);
                }
                None => {
                    live.rooms.remove(&change.owner);
                }
            }
        }
        for change in self.memberships {
            match change.value {
                Some(membership) => {
                    live.memberships.insert(change.member, membership);
                }
                None => {
                    live.memberships.remove(&change.member);
                }
            }
        }
        for change in self.generations {
            match change.value {
                Some(identity) => {
                    live.generations.insert(change.user_no, identity);
                }
                None => {
                    live.generations.remove(&change.user_no);
                }
            }
        }
        live.revision = self.next_revision;
        Ok(self.outcome)
    }
}

#[derive(Debug, Error)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the TCP MyRoom command tranche exercises remaining rejection variants"
    )
)]
pub(crate) enum MyRoomHubError {
    #[error(transparent)]
    Wire(#[from] MyRoomProtocolError),

    #[error("MyRoom player snapshot user number {snapshot} does not match identity {identity:?}")]
    SnapshotUserNoMismatch { identity: UserNo, snapshot: u32 },

    #[error("MyRoom player snapshot nickname {snapshot:?} does not match identity {identity:?}")]
    SnapshotNicknameMismatch { identity: String, snapshot: String },

    #[error("MyRoom owned by {owner:?} is full")]
    Full { owner: UserNo },

    #[error("identity {user_no:?} is not a MyRoom member")]
    NotMember { user_no: UserNo },

    #[error(
        "identity {user_no:?} has stale MyRoom generation {actual}; canonical generation is {expected}"
    )]
    StaleGeneration {
        user_no: UserNo,
        expected: u64,
        actual: u64,
    },

    #[error("identity {user_no:?} generation {generation} does not match its canonical binding")]
    IdentityBindingMismatch { user_no: UserNo, generation: u64 },

    #[error("identity advance changes user number from {previous:?} to {replacement:?}")]
    IdentityAdvanceUserMismatch {
        previous: UserNo,
        replacement: UserNo,
    },

    #[error("identity advance changes nickname from {previous:?} to {replacement:?}")]
    IdentityAdvanceNicknameMismatch {
        previous: String,
        replacement: String,
    },

    #[error(
        "identity {user_no:?} generation must advance beyond {previous}, received {replacement}"
    )]
    NonAdvancingGeneration {
        user_no: UserNo,
        previous: u64,
        replacement: u64,
    },

    #[error("MyRoom identity capacity {maximum} would be exceeded by {requested}")]
    IdentityCapacity { maximum: usize, requested: usize },

    #[error("MyRoom owned by {owner:?} does not exist")]
    RoomMissing { owner: UserNo },

    #[error("identity {user_no:?} is not present in its owned MyRoom")]
    NotPresentOwner { user_no: UserNo },

    #[error("MyRoom revision counter is exhausted")]
    RevisionExhausted,

    #[error(
        "MyRoom transition exceeds bounds: rooms={rooms}, memberships={memberships}, generations={generations}"
    )]
    TransitionTooLarge {
        rooms: usize,
        memberships: usize,
        generations: usize,
    },

    #[error("MyRoom internal invariant failed: {0}")]
    Invariant(#[from] MyRoomInvariantViolation),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the TCP MyRoom command tranche exercises the full invariant audit"
    )
)]
pub(crate) enum MyRoomInvariantViolation {
    #[error("room {owner:?} has no mapped members")]
    EmptyRoom { owner: UserNo },

    #[error("room {owner:?} owner presentation belongs to {actual:?}")]
    WrongOwnerPresentation { owner: UserNo, actual: UserNo },

    #[error("room {owner:?} slot {slot} has an invalid player snapshot")]
    InvalidPlayerSnapshot { owner: UserNo, slot: u8 },

    #[error("room {owner:?} has wire-invalid MyRoom info")]
    InvalidRoomInfo { owner: UserNo },

    #[error("room {owner:?} contains duplicate member {member:?}")]
    DuplicateRoomMember { owner: UserNo, member: UserNo },

    #[error("room {owner:?} slot {slot} lacks reverse membership")]
    MissingReverseMembership {
        owner: UserNo,
        slot: u8,
        member: UserNo,
    },

    #[error("member {member:?} points at missing room {owner:?}")]
    MissingMembershipRoom { member: UserNo, owner: UserNo },

    #[error("member {member:?} points at the wrong room slot {slot}")]
    MembershipSlotMismatch { member: UserNo, slot: u8 },

    #[error("tracked role for {user_no:?} lacks a canonical generation")]
    MissingCanonicalGeneration { user_no: UserNo },

    #[error("tracked role for {user_no:?} generation {actual} disagrees with canonical {expected}")]
    CanonicalGenerationMismatch {
        user_no: UserNo,
        expected: u64,
        actual: u64,
    },

    #[error("canonical generation for {user_no:?} has no room role")]
    OrphanCanonicalGeneration { user_no: UserNo },

    #[error("canonical generation table has {actual} identities; maximum is {maximum}")]
    IdentityCapacityExceeded { actual: usize, maximum: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Membership {
    owner: OwnerKey,
    slot: MyRoomSlotIndex,
}

#[derive(Debug, Clone)]
struct RoomState {
    owner: MyRoomParticipant,
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "the TCP MyRoom info command reads this field")
    )]
    info: MyRoomInfo,
    owner_present: bool,
    visitors: [Option<MyRoomParticipant>; VISITOR_CAPACITY],
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the TCP MyRoom entry command consumes remaining room mutations"
    )
)]
impl RoomState {
    fn new(owner: MyRoomOwner) -> Self {
        Self {
            owner: owner.participant,
            info: owner.info,
            owner_present: false,
            visitors: std::array::from_fn(|_| None),
        }
    }

    fn mapped_count(&self) -> usize {
        usize::from(self.owner_present) + self.visitors.iter().flatten().count()
    }

    fn next_slot_for(&self, user_no: UserNo) -> Option<MyRoomSlotIndex> {
        if user_no == self.owner.identity.user_no {
            return Some(MyRoomSlotIndex::OWNER);
        }
        self.visitors
            .iter()
            .position(Option::is_none)
            .map(|index| VISITOR_SLOTS[index])
    }

    fn participant(&self, slot: MyRoomSlotIndex) -> Option<&MyRoomParticipant> {
        if slot == MyRoomSlotIndex::OWNER {
            return self.owner_present.then_some(&self.owner);
        }
        self.visitors
            .get(slot.visitor_index()?)
            .and_then(Option::as_ref)
    }

    /// Adds membership without refreshing an existing authoritative owner
    /// presentation. Owner/profile refreshes use explicit operations.
    fn add_member(
        &mut self,
        slot: MyRoomSlotIndex,
        participant: MyRoomParticipant,
    ) -> Result<(), MyRoomInvariantViolation> {
        if slot == MyRoomSlotIndex::OWNER {
            if participant.identity != self.owner.identity {
                return Err(MyRoomInvariantViolation::CanonicalGenerationMismatch {
                    user_no: participant.identity.user_no,
                    expected: self.owner.identity.generation.get(),
                    actual: participant.identity.generation.get(),
                });
            }
            self.owner_present = true;
            return Ok(());
        }
        let Some(index) = slot.visitor_index() else {
            return Err(MyRoomInvariantViolation::MembershipSlotMismatch {
                member: participant.identity.user_no,
                slot: slot.get(),
            });
        };
        if self.visitors[index].is_some() {
            return Err(MyRoomInvariantViolation::MembershipSlotMismatch {
                member: participant.identity.user_no,
                slot: slot.get(),
            });
        }
        self.visitors[index] = Some(participant);
        Ok(())
    }

    fn replace_participant(
        &mut self,
        slot: MyRoomSlotIndex,
        participant: MyRoomParticipant,
    ) -> Result<(), MyRoomInvariantViolation> {
        if slot == MyRoomSlotIndex::OWNER {
            if !self.owner_present || participant.identity.user_no != self.owner.identity.user_no {
                return Err(MyRoomInvariantViolation::MembershipSlotMismatch {
                    member: participant.identity.user_no,
                    slot: slot.get(),
                });
            }
            self.owner = participant;
            return Ok(());
        }
        let Some(index) = slot.visitor_index() else {
            return Err(MyRoomInvariantViolation::MembershipSlotMismatch {
                member: participant.identity.user_no,
                slot: slot.get(),
            });
        };
        match self.visitors[index].as_ref() {
            Some(current) if current.identity.user_no == participant.identity.user_no => {}
            _ => {
                return Err(MyRoomInvariantViolation::MembershipSlotMismatch {
                    member: participant.identity.user_no,
                    slot: slot.get(),
                });
            }
        }
        self.visitors[index] = Some(participant);
        Ok(())
    }

    fn replace_owner_presentation(
        &mut self,
        participant: MyRoomParticipant,
    ) -> Result<(), MyRoomInvariantViolation> {
        if participant.identity.user_no != self.owner.identity.user_no {
            return Err(MyRoomInvariantViolation::WrongOwnerPresentation {
                owner: self.owner.identity.user_no,
                actual: participant.identity.user_no,
            });
        }
        self.owner = participant;
        Ok(())
    }

    fn remove_participant(
        &mut self,
        slot: MyRoomSlotIndex,
        expected: UserNo,
    ) -> Result<(), MyRoomInvariantViolation> {
        if slot == MyRoomSlotIndex::OWNER {
            if !self.owner_present || self.owner.identity.user_no != expected {
                return Err(MyRoomInvariantViolation::MembershipSlotMismatch {
                    member: expected,
                    slot: slot.get(),
                });
            }
            self.owner_present = false;
            return Ok(());
        }
        let Some(index) = slot.visitor_index() else {
            return Err(MyRoomInvariantViolation::MembershipSlotMismatch {
                member: expected,
                slot: slot.get(),
            });
        };
        match self.visitors[index].as_ref() {
            Some(participant) if participant.identity.user_no == expected => {
                self.visitors[index] = None;
                Ok(())
            }
            _ => Err(MyRoomInvariantViolation::MembershipSlotMismatch {
                member: expected,
                slot: slot.get(),
            }),
        }
    }

    fn snapshot(&self, key: OwnerKey) -> MyRoomSnapshot {
        let slots = std::array::from_fn(|index| {
            if index == 0 {
                MyRoomSlot::Player(self.owner.player.clone())
            } else {
                self.visitors[index - 1]
                    .as_ref()
                    .map_or(MyRoomSlot::Empty, |participant| {
                        MyRoomSlot::Player(participant.player.clone())
                    })
            }
        });
        MyRoomSnapshot {
            owner: key.user_no(),
            slots,
        }
    }

    fn audience(&self) -> Vec<IdentityBinding> {
        let mut audience = Vec::with_capacity(self.mapped_count());
        if self.owner_present {
            audience.push(self.owner.identity.clone());
        }
        audience.extend(
            self.visitors
                .iter()
                .flatten()
                .map(|participant| participant.identity.clone()),
        );
        audience
    }

    fn mapped_participants(&self) -> impl Iterator<Item = &MyRoomParticipant> {
        self.owner_present
            .then_some(&self.owner)
            .into_iter()
            .chain(self.visitors.iter().flatten())
    }
}

#[derive(Debug)]
struct RoomChange {
    owner: OwnerKey,
    value: Option<RoomState>,
}

#[derive(Debug)]
struct MembershipChange {
    member: UserNo,
    value: Option<Membership>,
}

#[derive(Debug)]
struct GenerationChange {
    user_no: UserNo,
    value: Option<IdentityBinding>,
}

#[derive(Clone, Copy)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "the TCP MyRoom entry command consumes this plan")
)]
struct EnterPlan<'a> {
    member: &'a MyRoomParticipant,
    owner: &'a MyRoomOwner,
    destination: OwnerKey,
    destination_slot: MyRoomSlotIndex,
    current: Option<Membership>,
    owner_is_new: bool,
    member_is_new: bool,
    next_revision: MyRoomRevision,
}

#[derive(Clone, Copy)]
struct DisconnectPlan<'a> {
    identity: &'a IdentityBinding,
    membership: Option<Membership>,
    owned_key: OwnerKey,
    owned: Option<&'a RoomState>,
    next_revision: MyRoomRevision,
}

/// Deterministic state intended to live exclusively inside the world actor.
#[derive(Debug)]
pub(crate) struct MyRoomHub {
    revision: MyRoomRevision,
    rooms: HashMap<OwnerKey, RoomState>,
    memberships: HashMap<UserNo, Membership>,
    /// One authoritative full binding per tracked user number.
    generations: HashMap<UserNo, IdentityBinding>,
    identity_capacity: NonZeroUsize,
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the TCP MyRoom command tranche consumes remaining request APIs"
    )
)]
impl MyRoomHub {
    /// Constructs a hub whose identity bound matches the runtime admission
    /// limit. Production callers cannot silently fall back to a fixed cap.
    #[must_use]
    pub(crate) fn with_identity_capacity(identity_capacity: NonZeroUsize) -> Self {
        Self {
            revision: MyRoomRevision::default(),
            rooms: HashMap::new(),
            memberships: HashMap::new(),
            generations: HashMap::new(),
            identity_capacity,
        }
    }

    #[cfg(test)]
    #[must_use]
    fn new() -> Self {
        Self::with_identity_capacity(
            NonZeroUsize::new(MAX_MYROOM_IDENTITIES)
                .expect("the test MyRoom identity capacity is nonzero"),
        )
    }

    #[must_use]
    pub(crate) fn revision(&self) -> MyRoomRevision {
        self.revision
    }

    #[must_use]
    pub(crate) fn room_count(&self) -> usize {
        self.rooms.len()
    }

    #[must_use]
    pub(crate) fn member_count(&self) -> usize {
        self.memberships.len()
    }

    #[must_use]
    pub(crate) fn generation_count(&self) -> usize {
        self.generations.len()
    }

    #[must_use]
    pub(crate) const fn identity_capacity(&self) -> NonZeroUsize {
        self.identity_capacity
    }

    pub(crate) fn enter(
        &self,
        member: &MyRoomParticipant,
        owner: &MyRoomOwner,
    ) -> Result<MyRoomTransition<EnterOutcome>, MyRoomHubError> {
        let destination = owner.key();
        let owner_is_new = self.validate_or_new_identity(&owner.participant.identity)?;
        let member_is_new = self.validate_or_new_identity(&member.identity)?;

        let current = self.memberships.get(&member.identity.user_no).copied();
        if let Some(membership) = current {
            self.require_exact_member(&member.identity, membership)?;
            if membership.owner == destination {
                let room = self.room_exact_owner(destination, &owner.participant.identity)?;
                let publication = publication(destination, room);
                return self.transition(
                    self.revision,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    EnterOutcome::Reentered {
                        slot: membership.slot,
                        publication: Box::new(publication),
                    },
                );
            }
        }

        let destination_slot = match self.rooms.get(&destination) {
            Some(room) => {
                self.require_canonical_identity(&room.owner.identity)?;
                self.require_canonical_identity(&owner.participant.identity)?;
                room.next_slot_for(member.identity.user_no)
                    .ok_or(MyRoomHubError::Full {
                        owner: destination.user_no(),
                    })?
            }
            None => {
                if member.identity.user_no == destination.user_no() {
                    MyRoomSlotIndex::OWNER
                } else {
                    MyRoomSlotIndex(1)
                }
            }
        };
        let additional_identities = usize::from(owner_is_new)
            + usize::from(
                member_is_new && member.identity.user_no != owner.participant.identity.user_no,
            );
        let pruned_identities = usize::from(current.is_some_and(|membership| {
            self.enter_prunes_old_owner_generation(membership, member.identity.user_no)
        }));
        self.validate_identity_capacity(additional_identities, pruned_identities)?;
        let next_revision = self.next_revision()?;
        self.build_enter_transition(EnterPlan {
            member,
            owner,
            destination,
            destination_slot,
            current,
            owner_is_new,
            member_is_new,
            next_revision,
        })
    }

    fn build_enter_transition(
        &self,
        plan: EnterPlan<'_>,
    ) -> Result<MyRoomTransition<EnterOutcome>, MyRoomHubError> {
        // All rejection checks above are allocation-free. Accepted work clones
        // at most the old and destination rooms.
        let mut room_changes = Vec::with_capacity(MAX_TRANSITION_ROOMS);
        let mut membership_changes = Vec::with_capacity(1);
        let mut generation_changes = Vec::with_capacity(2);
        let mut previous = None;

        if let Some(membership) = plan.current {
            let mut old_room = self.room(membership.owner)?.clone();
            old_room.remove_participant(membership.slot, plan.member.identity.user_no)?;
            if old_room.mapped_count() == 0 {
                previous = Some(RoomEffect::Deleted {
                    owner: membership.owner.user_no(),
                });
                room_changes.push(RoomChange {
                    owner: membership.owner,
                    value: None,
                });
            } else {
                let old_publication = publication(membership.owner, &old_room);
                previous = Some(RoomEffect::Updated(Box::new(old_publication)));
                room_changes.push(RoomChange {
                    owner: membership.owner,
                    value: Some(old_room),
                });
            }
        }

        let mut destination_room = match self.rooms.get(&plan.destination) {
            Some(room) => room.clone(),
            None => RoomState::new(plan.owner.clone()),
        };
        destination_room.add_member(plan.destination_slot, plan.member.clone())?;
        let current_publication = publication(plan.destination, &destination_room);
        room_changes.push(RoomChange {
            owner: plan.destination,
            value: Some(destination_room),
        });
        membership_changes.push(MembershipChange {
            member: plan.member.identity.user_no,
            value: Some(Membership {
                owner: plan.destination,
                slot: plan.destination_slot,
            }),
        });

        if plan.owner_is_new {
            push_generation_set(
                &mut generation_changes,
                plan.owner.participant.identity.clone(),
            );
        }
        if plan.member_is_new {
            push_generation_set(&mut generation_changes, plan.member.identity.clone());
        }
        if let Some(RoomEffect::Deleted { owner: old_owner }) = previous.as_ref() {
            self.push_generation_prune(
                &mut generation_changes,
                *old_owner,
                &room_changes,
                &membership_changes,
            );
        }

        self.transition(
            plan.next_revision,
            room_changes,
            membership_changes,
            generation_changes,
            EnterOutcome::Moved {
                slot: plan.destination_slot,
                previous,
                current: Box::new(current_publication),
            },
        )
    }

    pub(crate) fn membership_owner(
        &self,
        identity: &IdentityBinding,
    ) -> Result<UserNo, MyRoomHubError> {
        Ok(self.membership_exact(identity)?.owner.user_no())
    }

    /// Returns membership only when the supplied full binding is the current
    /// tracked generation. An identity which has never entered a `MyRoom` is a
    /// normal `None`; a stale or forged binding is rejected.
    pub(crate) fn membership_if_member(
        &self,
        identity: &IdentityBinding,
    ) -> Result<Option<MyRoomMembershipInfo>, MyRoomHubError> {
        let membership = self.memberships.get(&identity.user_no).copied();
        let owns_room = self.rooms.contains_key(&OwnerKey(identity.user_no));
        if membership.is_none() && !owns_room {
            if self.generations.contains_key(&identity.user_no) {
                return Err(MyRoomInvariantViolation::OrphanCanonicalGeneration {
                    user_no: identity.user_no,
                }
                .into());
            }
            return Ok(None);
        }
        let canonical = self.generations.get(&identity.user_no).ok_or(
            MyRoomInvariantViolation::MissingCanonicalGeneration {
                user_no: identity.user_no,
            },
        )?;
        require_same_binding(canonical, identity)?;
        let Some(membership) = membership else {
            return Ok(None);
        };
        self.require_exact_member(identity, membership)?;
        Ok(Some(MyRoomMembershipInfo {
            owner: membership.owner.user_no(),
            slot: membership.slot,
        }))
    }

    pub(crate) fn first_snapshot(
        &self,
        identity: &IdentityBinding,
    ) -> Result<MyRoomSnapshot, MyRoomHubError> {
        let membership = self.membership_exact(identity)?;
        Ok(self.room(membership.owner)?.snapshot(membership.owner))
    }

    pub(crate) fn peer_audience(
        &self,
        identity: &IdentityBinding,
    ) -> Result<PeerAudience, MyRoomHubError> {
        let membership = self.membership_exact(identity)?;
        let peers = self
            .room(membership.owner)?
            .audience()
            .into_iter()
            .filter(|peer| peer.user_no != identity.user_no)
            .collect();
        Ok(PeerAudience {
            owner: membership.owner.user_no(),
            sender_slot: membership.slot,
            peers,
        })
    }

    /// Exact-generation UDP query with a non-member fast path.
    pub(crate) fn peer_audience_if_member(
        &self,
        identity: &IdentityBinding,
    ) -> Result<Option<PeerAudience>, MyRoomHubError> {
        let Some(membership) = self.membership_if_member(identity)? else {
            return Ok(None);
        };
        let peers = self
            .room(OwnerKey(membership.owner))?
            .audience()
            .into_iter()
            .filter(|peer| peer.user_no != identity.user_no)
            .collect();
        Ok(Some(PeerAudience {
            owner: membership.owner,
            sender_slot: membership.slot,
            peers,
        }))
    }

    #[must_use]
    pub(crate) fn room_info(&self, owner: UserNo) -> Option<&MyRoomInfo> {
        self.rooms.get(&OwnerKey(owner)).map(|room| &room.info)
    }

    pub(crate) fn update_owner_info(
        &self,
        identity: &IdentityBinding,
        info: MyRoomInfo,
    ) -> Result<MyRoomTransition<OwnerInfoUpdate>, MyRoomHubError> {
        validate_myroom_info(&info)?;
        let membership = self.membership_exact(identity)?;
        let owner = OwnerKey(identity.user_no);
        if membership.owner != owner || membership.slot != MyRoomSlotIndex::OWNER {
            return Err(MyRoomHubError::NotPresentOwner {
                user_no: identity.user_no,
            });
        }
        let next_revision = self.next_revision()?;
        let mut room = self.room(owner)?.clone();
        let previous = std::mem::replace(&mut room.info, info.clone());
        self.transition(
            next_revision,
            vec![RoomChange {
                owner,
                value: Some(room),
            }],
            Vec::new(),
            Vec::new(),
            OwnerInfoUpdate {
                owner: identity.user_no,
                previous,
                current: info,
            },
        )
    }

    /// Explicitly refreshes player presentation for every role held by one
    /// exact generation. Enter/reenter never performs this refresh implicitly.
    pub(crate) fn refresh_participant(
        &self,
        participant: &MyRoomParticipant,
    ) -> Result<MyRoomTransition<ParticipantRefreshEffects>, MyRoomHubError> {
        self.require_canonical_identity(&participant.identity)?;
        let next_revision = self.next_revision()?;
        let (rooms, publications) =
            self.replacement_rooms(participant.identity.user_no, participant)?;
        self.transition(
            next_revision,
            rooms,
            Vec::new(),
            Vec::new(),
            ParticipantRefreshEffects { publications },
        )
    }

    /// Atomically advances all roles for one migrated identity generation.
    pub(crate) fn advance_identity(
        &self,
        previous: &IdentityBinding,
        replacement: MyRoomParticipant,
    ) -> Result<MyRoomTransition<IdentityAdvanceEffects>, MyRoomHubError> {
        self.require_canonical_identity(previous)?;
        validate_identity_advance(previous, &replacement.identity)?;
        let next_revision = self.next_revision()?;
        let (rooms, publications) = self.replacement_rooms(previous.user_no, &replacement)?;
        self.transition(
            next_revision,
            rooms,
            Vec::new(),
            vec![GenerationChange {
                user_no: previous.user_no,
                value: Some(replacement.identity.clone()),
            }],
            IdentityAdvanceEffects {
                previous: previous.clone(),
                current: replacement.identity,
                publications,
            },
        )
    }

    /// Advances a migrated generation without requiring profile I/O.
    ///
    /// Every role retains its own presentation fields. Only fields controlled
    /// by the actor-minted identity are replaced: user number, nickname,
    /// generation/owner/channel binding, and the source IPv4 address encoded
    /// by the legacy `MyRoom` wire format. IPv6 sources intentionally map to the
    /// unspecified IPv4 address, matching the login-session projection.
    pub(crate) fn advance_migrated_identity_if_tracked(
        &self,
        previous: &IdentityBinding,
        replacement: &IdentityBinding,
    ) -> Result<Option<MyRoomTransition<IdentityAdvanceEffects>>, MyRoomHubError> {
        let has_role = self.memberships.contains_key(&previous.user_no)
            || self.rooms.contains_key(&OwnerKey(previous.user_no));
        if !has_role {
            if self.generations.contains_key(&previous.user_no) {
                return Err(MyRoomInvariantViolation::OrphanCanonicalGeneration {
                    user_no: previous.user_no,
                }
                .into());
            }
            return Ok(None);
        }
        self.require_canonical_identity(previous)?;
        validate_identity_advance(previous, replacement)?;

        let next_revision = self.next_revision()?;
        let (rooms, publications) =
            self.migrated_replacement_rooms(previous.user_no, replacement)?;
        Ok(Some(self.transition(
            next_revision,
            rooms,
            Vec::new(),
            vec![GenerationChange {
                user_no: previous.user_no,
                value: Some(replacement.clone()),
            }],
            IdentityAdvanceEffects {
                previous: previous.clone(),
                current: replacement.clone(),
                publications,
            },
        )?))
    }

    pub(crate) fn leave(
        &self,
        identity: &IdentityBinding,
    ) -> Result<MyRoomTransition<LeaveEffects>, MyRoomHubError> {
        let membership = self.membership_exact(identity)?;
        let next_revision = self.next_revision()?;
        let mut room = self.room(membership.owner)?.clone();
        room.remove_participant(membership.slot, identity.user_no)?;

        let (room_change, effect) = if room.mapped_count() == 0 {
            (
                RoomChange {
                    owner: membership.owner,
                    value: None,
                },
                RoomEffect::Deleted {
                    owner: membership.owner.user_no(),
                },
            )
        } else {
            let publication = publication(membership.owner, &room);
            (
                RoomChange {
                    owner: membership.owner,
                    value: Some(room),
                },
                RoomEffect::Updated(Box::new(publication)),
            )
        };
        let room_changes = vec![room_change];
        let membership_changes = vec![MembershipChange {
            member: identity.user_no,
            value: None,
        }];
        let mut generation_changes = Vec::with_capacity(2);
        self.push_generation_prune(
            &mut generation_changes,
            identity.user_no,
            &room_changes,
            &membership_changes,
        );
        self.push_generation_prune(
            &mut generation_changes,
            membership.owner.user_no(),
            &room_changes,
            &membership_changes,
        );
        self.transition(
            next_revision,
            room_changes,
            membership_changes,
            generation_changes,
            LeaveEffects { room: effect },
        )
    }

    pub(crate) fn disconnect(
        &self,
        identity: &IdentityBinding,
    ) -> Result<MyRoomTransition<MyRoomDisconnectOutcome>, MyRoomHubError> {
        let Some(canonical) = self.generations.get(&identity.user_no) else {
            return self.transition(
                self.revision,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                MyRoomDisconnectOutcome::NotTracked,
            );
        };
        require_same_binding(canonical, identity)?;

        let membership = self.memberships.get(&identity.user_no).copied();
        if let Some(membership) = membership {
            self.require_exact_member(identity, membership)?;
        }
        let owned_key = OwnerKey(identity.user_no);
        let owned = self.rooms.get(&owned_key);
        if let Some(room) = owned {
            require_same_binding(&room.owner.identity, identity)?;
            for participant in room.mapped_participants() {
                let mapped = self.memberships.get(&participant.identity.user_no).ok_or(
                    MyRoomInvariantViolation::MissingReverseMembership {
                        owner: identity.user_no,
                        slot: 0,
                        member: participant.identity.user_no,
                    },
                )?;
                if mapped.owner != owned_key {
                    return Err(MyRoomInvariantViolation::MissingReverseMembership {
                        owner: identity.user_no,
                        slot: mapped.slot.get(),
                        member: participant.identity.user_no,
                    }
                    .into());
                }
            }
        }
        if membership.is_none() && owned.is_none() {
            return Err(MyRoomInvariantViolation::OrphanCanonicalGeneration {
                user_no: identity.user_no,
            }
            .into());
        }
        let next_revision = self.next_revision()?;
        self.build_disconnect_transition(DisconnectPlan {
            identity,
            membership,
            owned_key,
            owned,
            next_revision,
        })
    }

    /// Plans cleanup from an identity-registry release capability.
    ///
    /// `ReleasedIdentity` deliberately has no session owner. All other
    /// actor-minted fields must still match the canonical `MyRoom` generation,
    /// so an expired or reconstructed stamp cannot close a newer room.
    pub(crate) fn disconnect_released(
        &self,
        released: &ReleasedIdentity,
    ) -> Result<MyRoomTransition<MyRoomDisconnectOutcome>, MyRoomHubError> {
        let has_role = self.memberships.contains_key(&released.user_no)
            || self.rooms.contains_key(&OwnerKey(released.user_no));
        if !has_role {
            if self.generations.contains_key(&released.user_no) {
                return Err(MyRoomInvariantViolation::OrphanCanonicalGeneration {
                    user_no: released.user_no,
                }
                .into());
            }
            return self.transition(
                self.revision,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                MyRoomDisconnectOutcome::NotTracked,
            );
        }
        let canonical = self.generations.get(&released.user_no).ok_or(
            MyRoomInvariantViolation::MissingCanonicalGeneration {
                user_no: released.user_no,
            },
        )?;
        require_same_released_binding(canonical, released)?;
        self.disconnect(canonical)
    }

    fn build_disconnect_transition(
        &self,
        plan: DisconnectPlan<'_>,
    ) -> Result<MyRoomTransition<MyRoomDisconnectOutcome>, MyRoomHubError> {
        let DisconnectPlan {
            identity,
            membership,
            owned_key,
            owned,
            next_revision,
        } = plan;
        let mut room_changes = Vec::with_capacity(MAX_TRANSITION_ROOMS);
        let mut membership_changes = Vec::with_capacity(MAX_TRANSITION_MEMBERSHIPS);
        let mut generation_changes = Vec::with_capacity(MAX_TRANSITION_GENERATIONS);
        let mut membership_room = None;

        if let Some(membership) = membership {
            membership_changes.push(MembershipChange {
                member: identity.user_no,
                value: None,
            });
            if membership.owner != owned_key || owned.is_none() {
                let mut room = self.room(membership.owner)?.clone();
                room.remove_participant(membership.slot, identity.user_no)?;
                if room.mapped_count() == 0 {
                    membership_room = Some(RoomEffect::Deleted {
                        owner: membership.owner.user_no(),
                    });
                    room_changes.push(RoomChange {
                        owner: membership.owner,
                        value: None,
                    });
                } else {
                    let room_publication = publication(membership.owner, &room);
                    membership_room = Some(RoomEffect::Updated(Box::new(room_publication)));
                    room_changes.push(RoomChange {
                        owner: membership.owner,
                        value: Some(room),
                    });
                }
            }
        }

        let closed_owned_room = if let Some(room) = owned {
            let ejected: Vec<_> = room
                .mapped_participants()
                .filter(|participant| participant.identity.user_no != identity.user_no)
                .map(|participant| participant.identity.clone())
                .collect();
            for participant in &ejected {
                push_membership_delete(&mut membership_changes, participant.user_no);
            }
            room_changes.push(RoomChange {
                owner: owned_key,
                value: None,
            });
            Some(ClosedOwnedRoom {
                owner: identity.user_no,
                ejected,
            })
        } else {
            None
        };

        let mut affected_users = Vec::with_capacity(MAX_TRANSITION_GENERATIONS);
        affected_users.push(identity.user_no);
        if let Some(closed) = &closed_owned_room {
            affected_users.extend(closed.ejected.iter().map(|participant| participant.user_no));
        }
        if let Some(RoomEffect::Deleted { owner }) = &membership_room {
            affected_users.push(*owner);
        }
        for user_no in affected_users {
            self.push_generation_prune(
                &mut generation_changes,
                user_no,
                &room_changes,
                &membership_changes,
            );
        }

        self.transition(
            next_revision,
            room_changes,
            membership_changes,
            generation_changes,
            MyRoomDisconnectOutcome::Applied(DisconnectEffects {
                membership_room,
                closed_owned_room,
            }),
        )
    }

    fn replacement_rooms(
        &self,
        user_no: UserNo,
        replacement: &MyRoomParticipant,
    ) -> Result<(Vec<RoomChange>, Vec<RoomPublication>), MyRoomHubError> {
        let membership = self.memberships.get(&user_no).copied();
        let owned_key = OwnerKey(user_no);
        let owns_room = self.rooms.contains_key(&owned_key);
        if membership.is_none() && !owns_room {
            return Err(MyRoomInvariantViolation::OrphanCanonicalGeneration { user_no }.into());
        }

        let mut changes = Vec::with_capacity(MAX_TRANSITION_ROOMS);
        let mut publications = Vec::with_capacity(MAX_TRANSITION_ROOMS);
        if let Some(membership) = membership {
            let mut room = self.room(membership.owner)?.clone();
            room.replace_participant(membership.slot, replacement.clone())?;
            publications.push(publication(membership.owner, &room));
            changes.push(RoomChange {
                owner: membership.owner,
                value: Some(room),
            });
        }
        if owns_room && membership.is_none_or(|mapped| mapped.owner != owned_key) {
            let mut room = self.room(owned_key)?.clone();
            room.replace_owner_presentation(replacement.clone())?;
            publications.push(publication(owned_key, &room));
            changes.push(RoomChange {
                owner: owned_key,
                value: Some(room),
            });
        }
        Ok((changes, publications))
    }

    fn migrated_replacement_rooms(
        &self,
        user_no: UserNo,
        replacement: &IdentityBinding,
    ) -> Result<(Vec<RoomChange>, Vec<RoomPublication>), MyRoomHubError> {
        let membership = self.memberships.get(&user_no).copied();
        let owned_key = OwnerKey(user_no);
        let owns_room = self.rooms.contains_key(&owned_key);
        if membership.is_none() && !owns_room {
            return Err(MyRoomInvariantViolation::OrphanCanonicalGeneration { user_no }.into());
        }

        let mut changes = Vec::with_capacity(MAX_TRANSITION_ROOMS);
        let mut publications = Vec::with_capacity(MAX_TRANSITION_ROOMS);
        if let Some(membership) = membership {
            let mut room = self.room(membership.owner)?.clone();
            let current = room.participant(membership.slot).ok_or(
                MyRoomInvariantViolation::MembershipSlotMismatch {
                    member: user_no,
                    slot: membership.slot.get(),
                },
            )?;
            let migrated = migrated_participant(current, replacement)?;
            room.replace_participant(membership.slot, migrated)?;
            publications.push(publication(membership.owner, &room));
            changes.push(RoomChange {
                owner: membership.owner,
                value: Some(room),
            });
        }
        if owns_room && membership.is_none_or(|mapped| mapped.owner != owned_key) {
            let mut room = self.room(owned_key)?.clone();
            let migrated = migrated_participant(&room.owner, replacement)?;
            room.replace_owner_presentation(migrated)?;
            publications.push(publication(owned_key, &room));
            changes.push(RoomChange {
                owner: owned_key,
                value: Some(room),
            });
        }
        Ok((changes, publications))
    }

    fn membership_exact(&self, identity: &IdentityBinding) -> Result<Membership, MyRoomHubError> {
        self.require_canonical_identity(identity)?;
        let membership =
            *self
                .memberships
                .get(&identity.user_no)
                .ok_or(MyRoomHubError::NotMember {
                    user_no: identity.user_no,
                })?;
        self.require_exact_member(identity, membership)?;
        Ok(membership)
    }

    fn require_exact_member(
        &self,
        identity: &IdentityBinding,
        membership: Membership,
    ) -> Result<(), MyRoomHubError> {
        let participant = self
            .room(membership.owner)?
            .participant(membership.slot)
            .ok_or(MyRoomInvariantViolation::MembershipSlotMismatch {
                member: identity.user_no,
                slot: membership.slot.get(),
            })?;
        if participant.identity.user_no != identity.user_no {
            return Err(MyRoomInvariantViolation::MembershipSlotMismatch {
                member: identity.user_no,
                slot: membership.slot.get(),
            }
            .into());
        }
        require_same_binding(&participant.identity, identity)
    }

    fn room(&self, owner: OwnerKey) -> Result<&RoomState, MyRoomHubError> {
        self.rooms.get(&owner).ok_or(MyRoomHubError::RoomMissing {
            owner: owner.user_no(),
        })
    }

    fn room_exact_owner(
        &self,
        owner: OwnerKey,
        identity: &IdentityBinding,
    ) -> Result<&RoomState, MyRoomHubError> {
        let room = self.room(owner)?;
        self.require_canonical_identity(identity)?;
        require_same_binding(&room.owner.identity, identity)?;
        Ok(room)
    }

    fn validate_or_new_identity(&self, identity: &IdentityBinding) -> Result<bool, MyRoomHubError> {
        match self.generations.get(&identity.user_no) {
            Some(canonical) => {
                require_same_binding(canonical, identity)?;
                Ok(false)
            }
            None => Ok(true),
        }
    }

    fn require_canonical_identity(&self, identity: &IdentityBinding) -> Result<(), MyRoomHubError> {
        let canonical = self.generations.get(&identity.user_no).ok_or(
            MyRoomInvariantViolation::MissingCanonicalGeneration {
                user_no: identity.user_no,
            },
        )?;
        require_same_binding(canonical, identity)
    }

    fn enter_prunes_old_owner_generation(
        &self,
        membership: Membership,
        moving_user: UserNo,
    ) -> bool {
        let old_owner = membership.owner.user_no();
        old_owner != moving_user
            && self.generations.contains_key(&old_owner)
            && !self.memberships.contains_key(&old_owner)
            && self
                .rooms
                .get(&membership.owner)
                .is_some_and(|room| room.mapped_count() == 1)
    }

    fn validate_identity_capacity(
        &self,
        additional: usize,
        pruned: usize,
    ) -> Result<(), MyRoomHubError> {
        let requested = self
            .generations
            .len()
            .saturating_add(additional)
            .saturating_sub(pruned);
        let maximum = self.identity_capacity.get();
        if requested > maximum {
            Err(MyRoomHubError::IdentityCapacity { maximum, requested })
        } else {
            Ok(())
        }
    }

    fn next_revision(&self) -> Result<MyRoomRevision, MyRoomHubError> {
        self.revision
            .get()
            .checked_add(1)
            .map(MyRoomRevision)
            .ok_or(MyRoomHubError::RevisionExhausted)
    }

    fn transition<T>(
        &self,
        next_revision: MyRoomRevision,
        rooms: Vec<RoomChange>,
        memberships: Vec<MembershipChange>,
        generations: Vec<GenerationChange>,
        outcome: T,
    ) -> Result<MyRoomTransition<T>, MyRoomHubError> {
        if rooms.len() > MAX_TRANSITION_ROOMS
            || memberships.len() > MAX_TRANSITION_MEMBERSHIPS
            || generations.len() > MAX_TRANSITION_GENERATIONS
        {
            return Err(MyRoomHubError::TransitionTooLarge {
                rooms: rooms.len(),
                memberships: memberships.len(),
                generations: generations.len(),
            });
        }
        Ok(MyRoomTransition {
            base_revision: self.revision,
            next_revision,
            rooms,
            memberships,
            generations,
            outcome,
        })
    }

    fn push_generation_prune(
        &self,
        changes: &mut Vec<GenerationChange>,
        user_no: UserNo,
        rooms: &[RoomChange],
        memberships: &[MembershipChange],
    ) {
        if changes.iter().any(|change| change.user_no == user_no) {
            return;
        }
        if self.generations.contains_key(&user_no)
            && !self.user_referenced_after(user_no, rooms, memberships)
        {
            changes.push(GenerationChange {
                user_no,
                value: None,
            });
        }
    }

    fn user_referenced_after(
        &self,
        user_no: UserNo,
        rooms: &[RoomChange],
        memberships: &[MembershipChange],
    ) -> bool {
        membership_after(self, memberships, user_no).is_some()
            || room_after(self, rooms, OwnerKey(user_no)).is_some()
    }

    pub(crate) fn audit_invariants(&self) -> Result<(), MyRoomInvariantViolation> {
        let maximum = self.identity_capacity.get();
        if self.generations.len() > maximum {
            return Err(MyRoomInvariantViolation::IdentityCapacityExceeded {
                actual: self.generations.len(),
                maximum,
            });
        }

        for (owner, room) in &self.rooms {
            if room.mapped_count() == 0 {
                return Err(MyRoomInvariantViolation::EmptyRoom {
                    owner: owner.user_no(),
                });
            }
            if room.owner.identity.user_no != owner.user_no() {
                return Err(MyRoomInvariantViolation::WrongOwnerPresentation {
                    owner: owner.user_no(),
                    actual: room.owner.identity.user_no,
                });
            }
            if validate_myroom_info(&room.info).is_err() {
                return Err(MyRoomInvariantViolation::InvalidRoomInfo {
                    owner: owner.user_no(),
                });
            }
            validate_player_snapshot(owner.user_no(), 0, &room.owner)?;
            self.audit_canonical(&room.owner)?;

            let mut seen = HashSet::with_capacity(room.mapped_count());
            if room.owner_present {
                seen.insert(room.owner.identity.user_no);
                self.require_reverse_membership(*owner, MyRoomSlotIndex::OWNER, &room.owner)?;
            }
            for (index, participant) in room.visitors.iter().enumerate() {
                let Some(participant) = participant else {
                    continue;
                };
                let slot = VISITOR_SLOTS[index];
                validate_player_snapshot(owner.user_no(), slot.get(), participant)?;
                self.audit_canonical(participant)?;
                if participant.identity.user_no == owner.user_no()
                    || !seen.insert(participant.identity.user_no)
                {
                    return Err(MyRoomInvariantViolation::DuplicateRoomMember {
                        owner: owner.user_no(),
                        member: participant.identity.user_no,
                    });
                }
                self.require_reverse_membership(*owner, slot, participant)?;
            }
        }

        for (member, membership) in &self.memberships {
            let room = self.rooms.get(&membership.owner).ok_or(
                MyRoomInvariantViolation::MissingMembershipRoom {
                    member: *member,
                    owner: membership.owner.user_no(),
                },
            )?;
            let participant = room.participant(membership.slot).ok_or(
                MyRoomInvariantViolation::MembershipSlotMismatch {
                    member: *member,
                    slot: membership.slot.get(),
                },
            )?;
            if participant.identity.user_no != *member {
                return Err(MyRoomInvariantViolation::MembershipSlotMismatch {
                    member: *member,
                    slot: membership.slot.get(),
                });
            }
            self.audit_canonical(participant)?;
        }

        for user_no in self.generations.keys() {
            if !self.memberships.contains_key(user_no)
                && !self.rooms.contains_key(&OwnerKey(*user_no))
            {
                return Err(MyRoomInvariantViolation::OrphanCanonicalGeneration {
                    user_no: *user_no,
                });
            }
        }
        Ok(())
    }

    fn audit_canonical(
        &self,
        participant: &MyRoomParticipant,
    ) -> Result<(), MyRoomInvariantViolation> {
        let canonical = self.generations.get(&participant.identity.user_no).ok_or(
            MyRoomInvariantViolation::MissingCanonicalGeneration {
                user_no: participant.identity.user_no,
            },
        )?;
        if canonical == &participant.identity {
            Ok(())
        } else {
            Err(MyRoomInvariantViolation::CanonicalGenerationMismatch {
                user_no: participant.identity.user_no,
                expected: canonical.generation.get(),
                actual: participant.identity.generation.get(),
            })
        }
    }

    fn require_reverse_membership(
        &self,
        owner: OwnerKey,
        slot: MyRoomSlotIndex,
        participant: &MyRoomParticipant,
    ) -> Result<(), MyRoomInvariantViolation> {
        if self
            .memberships
            .get(&participant.identity.user_no)
            .is_some_and(|membership| membership.owner == owner && membership.slot == slot)
        {
            Ok(())
        } else {
            Err(MyRoomInvariantViolation::MissingReverseMembership {
                owner: owner.user_no(),
                slot: slot.get(),
                member: participant.identity.user_no,
            })
        }
    }
}

fn publication(owner: OwnerKey, room: &RoomState) -> RoomPublication {
    RoomPublication {
        owner: owner.user_no(),
        snapshot: room.snapshot(owner),
        audience: room.audience(),
    }
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the TCP MyRoom command audit consumes full snapshot validation"
    )
)]
fn validate_player_snapshot(
    owner: UserNo,
    slot: u8,
    participant: &MyRoomParticipant,
) -> Result<(), MyRoomInvariantViolation> {
    if validate_myroom_player_slot(&participant.player).is_err()
        || participant.player.user_no != participant.identity.user_no.get()
        || canonical_nickname_key(&participant.player.nickname)
            != canonical_nickname_key(&participant.identity.nickname)
    {
        Err(MyRoomInvariantViolation::InvalidPlayerSnapshot { owner, slot })
    } else {
        Ok(())
    }
}

fn require_same_binding(
    canonical: &IdentityBinding,
    claimed: &IdentityBinding,
) -> Result<(), MyRoomHubError> {
    if canonical.user_no != claimed.user_no || canonical.generation != claimed.generation {
        return Err(MyRoomHubError::StaleGeneration {
            user_no: claimed.user_no,
            expected: canonical.generation.get(),
            actual: claimed.generation.get(),
        });
    }
    if canonical != claimed {
        return Err(MyRoomHubError::IdentityBindingMismatch {
            user_no: claimed.user_no,
            generation: claimed.generation.get(),
        });
    }
    Ok(())
}

fn require_same_released_binding(
    canonical: &IdentityBinding,
    released: &ReleasedIdentity,
) -> Result<(), MyRoomHubError> {
    if canonical.user_no != released.user_no || canonical.generation != released.generation {
        return Err(MyRoomHubError::StaleGeneration {
            user_no: released.user_no,
            expected: canonical.generation.get(),
            actual: released.generation.get(),
        });
    }
    if canonical.nickname != released.nickname
        || canonical.source_ip != released.source_ip
        || canonical.channel != released.channel
    {
        return Err(MyRoomHubError::IdentityBindingMismatch {
            user_no: released.user_no,
            generation: released.generation.get(),
        });
    }
    Ok(())
}

fn validate_identity_advance(
    previous: &IdentityBinding,
    replacement: &IdentityBinding,
) -> Result<(), MyRoomHubError> {
    if previous.user_no != replacement.user_no {
        return Err(MyRoomHubError::IdentityAdvanceUserMismatch {
            previous: previous.user_no,
            replacement: replacement.user_no,
        });
    }
    if canonical_nickname_key(&previous.nickname) != canonical_nickname_key(&replacement.nickname) {
        return Err(MyRoomHubError::IdentityAdvanceNicknameMismatch {
            previous: previous.nickname.clone(),
            replacement: replacement.nickname.clone(),
        });
    }
    if replacement.generation.get() <= previous.generation.get() {
        return Err(MyRoomHubError::NonAdvancingGeneration {
            user_no: previous.user_no,
            previous: previous.generation.get(),
            replacement: replacement.generation.get(),
        });
    }
    Ok(())
}

fn migrated_participant(
    current: &MyRoomParticipant,
    replacement: &IdentityBinding,
) -> Result<MyRoomParticipant, MyRoomHubError> {
    let mut player = current.player.clone();
    player.user_no = replacement.user_no.get();
    player.nickname.clone_from(&replacement.nickname);
    player.p2p_address = match replacement.source_ip {
        IpAddr::V4(address) => address,
        IpAddr::V6(_) => Ipv4Addr::UNSPECIFIED,
    };
    MyRoomParticipant::new(replacement.clone(), player)
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the TCP MyRoom entry command consumes generation insertion"
    )
)]
fn push_generation_set(changes: &mut Vec<GenerationChange>, identity: IdentityBinding) {
    if let Some(change) = changes
        .iter_mut()
        .find(|change| change.user_no == identity.user_no)
    {
        change.value = Some(identity);
    } else {
        changes.push(GenerationChange {
            user_no: identity.user_no,
            value: Some(identity),
        });
    }
}

fn push_membership_delete(changes: &mut Vec<MembershipChange>, member: UserNo) {
    if !changes.iter().any(|change| change.member == member) {
        changes.push(MembershipChange {
            member,
            value: None,
        });
    }
}

fn room_after<'a>(
    hub: &'a MyRoomHub,
    changes: &'a [RoomChange],
    owner: OwnerKey,
) -> Option<&'a RoomState> {
    changes
        .iter()
        .rev()
        .find(|change| change.owner == owner)
        .map_or_else(|| hub.rooms.get(&owner), |change| change.value.as_ref())
}

fn membership_after(
    hub: &MyRoomHub,
    changes: &[MembershipChange],
    member: UserNo,
) -> Option<Membership> {
    changes
        .iter()
        .rev()
        .find(|change| change.member == member)
        .map_or_else(
            || hub.memberships.get(&member).copied(),
            |change| change.value,
        )
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr},
        num::NonZeroUsize,
        time::Instant,
    };

    use p5136_core::{
        myroom_protocol::{
            MAX_MYROOM_PASSWORD_UTF16_UNITS, MYROOM_SLOT_COUNT, MyRoomInfo, MyRoomPlayerSlot,
            MyRoomProtocolError, MyRoomSlot,
        },
        room_protocol::{MAX_CLUB_NAME_UTF16_UNITS, MAX_RIDER_NICKNAME_UTF16_UNITS},
        startup::RIDER_ITEM_SNAPSHOT_WIRE_LENGTH,
    };

    use super::{
        EnterOutcome, GenerationChange, IdentityAdvanceEffects, MAX_MYROOM_IDENTITIES,
        MAX_TRANSITION_GENERATIONS, MAX_TRANSITION_MEMBERSHIPS, MAX_TRANSITION_ROOMS,
        MembershipChange, MyRoomCommitError, MyRoomDisconnectOutcome, MyRoomHub, MyRoomHubError,
        MyRoomInvariantViolation, MyRoomOwner, MyRoomParticipant, MyRoomRevision, MyRoomSlotIndex,
        MyRoomTransition, OwnerKey, ParticipantRefreshEffects, RoomChange, RoomEffect, RoomState,
        VISITOR_CAPACITY,
    };
    use crate::{IdentityBinding, IdentityRegistry, ReleasedIdentity, SessionId};

    fn claim(registry: &mut IdentityRegistry, session: u64, nickname: &str) -> IdentityBinding {
        registry
            .claim(
                SessionId::new(session),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                nickname,
            )
            .unwrap()
    }

    fn replacement_identity(
        registry: &mut IdentityRegistry,
        old_session: u64,
        new_session: u64,
        nickname: &str,
    ) -> IdentityBinding {
        let _ = registry.disconnect(SessionId::new(old_session), Instant::now());
        claim(registry, new_session, nickname)
    }

    fn released(identity: &IdentityBinding) -> ReleasedIdentity {
        ReleasedIdentity {
            nickname: identity.nickname.clone(),
            user_no: identity.user_no,
            generation: identity.generation,
            source_ip: identity.source_ip,
            channel: identity.channel,
        }
    }

    fn participant(identity: &IdentityBinding) -> MyRoomParticipant {
        let user_u8 = identity.user_no.get().to_le_bytes()[0];
        let user_u16 = u16::try_from(identity.user_no.get()).unwrap();
        MyRoomParticipant::new(
            identity.clone(),
            MyRoomPlayerSlot {
                user_no: identity.user_no.get(),
                p2p_address: Ipv4Addr::new(192, 0, 2, user_u8),
                p2p_port: 30_000 + user_u16,
                nickname: identity.nickname.clone(),
                rider_item_snapshot: [user_u8; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
                rp: identity.user_no.get() * 100,
                club_name: format!("club-{}", identity.user_no.get()),
            },
        )
        .unwrap()
    }

    fn participant_with_rp(identity: &IdentityBinding, rp: u32) -> MyRoomParticipant {
        let mut player = participant(identity).player().clone();
        player.rp = rp;
        MyRoomParticipant::new(identity.clone(), player).unwrap()
    }

    fn owner(identity: &IdentityBinding) -> MyRoomOwner {
        let room_id = i16::try_from(identity.user_no.get()).unwrap();
        let bgm = identity.user_no.get().to_le_bytes()[0];
        MyRoomOwner::new(
            participant(identity),
            MyRoomInfo {
                room_id,
                bgm,
                ..MyRoomInfo::default()
            },
        )
        .unwrap()
    }

    fn owner_with_state(identity: &IdentityBinding, rp: u32, room_id: i16) -> MyRoomOwner {
        MyRoomOwner::new(
            participant_with_rp(identity, rp),
            MyRoomInfo {
                room_id,
                ..MyRoomInfo::default()
            },
        )
        .unwrap()
    }

    fn commit<T>(hub: &mut MyRoomHub, transition: MyRoomTransition<T>) -> T {
        transition.commit(hub).unwrap()
    }

    #[allow(clippy::needless_pass_by_value)]
    fn enter(
        hub: &mut MyRoomHub,
        member: MyRoomParticipant,
        owner: MyRoomOwner,
    ) -> Result<EnterOutcome, MyRoomHubError> {
        let transition = hub.enter(&member, &owner)?;
        Ok(commit(hub, transition))
    }

    fn leave(
        hub: &mut MyRoomHub,
        identity: &IdentityBinding,
    ) -> Result<super::LeaveEffects, MyRoomHubError> {
        let transition = hub.leave(identity)?;
        Ok(commit(hub, transition))
    }

    fn disconnect(
        hub: &mut MyRoomHub,
        identity: &IdentityBinding,
    ) -> Result<MyRoomDisconnectOutcome, MyRoomHubError> {
        let transition = hub.disconnect(identity)?;
        Ok(commit(hub, transition))
    }

    fn entered_slot(outcome: &EnterOutcome) -> MyRoomSlotIndex {
        match outcome {
            EnterOutcome::Reentered { slot, .. } | EnterOutcome::Moved { slot, .. } => *slot,
        }
    }

    fn user_numbers(audience: &[IdentityBinding]) -> Vec<u32> {
        audience
            .iter()
            .map(|identity| identity.user_no.get())
            .collect()
    }

    fn fill_with_owned_rooms(
        hub: &mut MyRoomHub,
        registry: &mut IdentityRegistry,
        first_session: u64,
        count: usize,
    ) {
        for offset in 0..count {
            let session = first_session + u64::try_from(offset).unwrap();
            let identity = claim(registry, session, &format!("CapacityFiller{session}"));
            enter(hub, participant(&identity), owner(&identity)).unwrap();
        }
    }

    #[test]
    fn allocates_owner_zero_and_lowest_available_visitor_slot() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let first = claim(&mut registry, 2, "First");
        let second = claim(&mut registry, 3, "Second");
        let replacement = claim(&mut registry, 4, "Replacement");
        let mut hub = MyRoomHub::new();

        assert_eq!(
            entered_slot(&enter(&mut hub, participant(&owner_id), owner(&owner_id)).unwrap()),
            MyRoomSlotIndex::OWNER
        );
        assert_eq!(
            entered_slot(&enter(&mut hub, participant(&first), owner(&owner_id)).unwrap()).get(),
            1
        );
        assert_eq!(
            entered_slot(&enter(&mut hub, participant(&second), owner(&owner_id)).unwrap()).get(),
            2
        );
        leave(&mut hub, &first).unwrap();
        assert_eq!(
            entered_slot(&enter(&mut hub, participant(&replacement), owner(&owner_id)).unwrap())
                .get(),
            1
        );
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn capacity_is_owner_plus_seven_visitors() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&owner_id), owner(&owner_id)).unwrap();
        for index in 0..VISITOR_CAPACITY {
            let visitor = claim(
                &mut registry,
                u64::try_from(index + 2).unwrap(),
                &format!("Visitor{index}"),
            );
            let outcome = enter(&mut hub, participant(&visitor), owner(&owner_id)).unwrap();
            assert_eq!(
                entered_slot(&outcome).get(),
                u8::try_from(index + 1).unwrap()
            );
        }
        assert_eq!(hub.member_count(), MYROOM_SLOT_COUNT);
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn full_destination_rolls_back_existing_membership() {
        let mut registry = IdentityRegistry::new();
        let full_owner = claim(&mut registry, 1, "FullOwner");
        let old_owner = claim(&mut registry, 2, "OldOwner");
        let candidate = claim(&mut registry, 3, "Candidate");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&candidate), owner(&old_owner)).unwrap();
        for index in 0..VISITOR_CAPACITY {
            let visitor = claim(
                &mut registry,
                u64::try_from(index + 10).unwrap(),
                &format!("Visitor{index}"),
            );
            enter(&mut hub, participant(&visitor), owner(&full_owner)).unwrap();
        }
        let revision = hub.revision();
        assert!(matches!(
            hub.enter(&participant(&candidate), &owner(&full_owner)),
            Err(MyRoomHubError::Full { .. })
        ));
        assert_eq!(hub.revision(), revision);
        assert_eq!(hub.membership_owner(&candidate).unwrap(), old_owner.user_no);
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn same_room_reentry_is_idempotent() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let visitor = claim(&mut registry, 2, "Visitor");
        let mut hub = MyRoomHub::new();
        let first = enter(&mut hub, participant(&visitor), owner(&owner_id)).unwrap();
        let revision = hub.revision();
        let second = enter(&mut hub, participant(&visitor), owner(&owner_id)).unwrap();
        assert_eq!(entered_slot(&first).get(), 1);
        assert!(matches!(second, EnterOutcome::Reentered { .. }));
        assert_eq!(hub.revision(), revision);
        assert_eq!(hub.member_count(), 1);
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn existing_room_enter_never_overwrites_authoritative_owner_state() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let first = claim(&mut registry, 2, "First");
        let second = claim(&mut registry, 3, "Second");
        let mut hub = MyRoomHub::new();
        enter(
            &mut hub,
            participant(&first),
            owner_with_state(&owner_id, 111, 10),
        )
        .unwrap();
        enter(
            &mut hub,
            participant(&second),
            owner_with_state(&owner_id, 999, 77),
        )
        .unwrap();

        assert_eq!(hub.room_info(owner_id.user_no).unwrap().room_id, 10);
        let snapshot = hub.first_snapshot(&first).unwrap();
        assert!(matches!(
            &snapshot.slots[0],
            MyRoomSlot::Player(player) if player.rp == 111
        ));
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn switch_returns_old_then_new_deterministic_publications() {
        let mut registry = IdentityRegistry::new();
        let old_owner = claim(&mut registry, 1, "OldOwner");
        let new_owner = claim(&mut registry, 2, "NewOwner");
        let mover = claim(&mut registry, 3, "Mover");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&old_owner), owner(&old_owner)).unwrap();
        enter(&mut hub, participant(&new_owner), owner(&new_owner)).unwrap();
        enter(&mut hub, participant(&mover), owner(&old_owner)).unwrap();
        let EnterOutcome::Moved {
            previous: Some(RoomEffect::Updated(previous)),
            current,
            ..
        } = enter(&mut hub, participant(&mover), owner(&new_owner)).unwrap()
        else {
            panic!("expected a two-room move");
        };
        assert_eq!(previous.owner, old_owner.user_no);
        assert_eq!(
            user_numbers(&previous.audience),
            vec![old_owner.user_no.get()]
        );
        assert_eq!(
            user_numbers(&current.audience),
            vec![new_owner.user_no.get(), mover.user_no.get()]
        );
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn owner_g1_presentation_rejects_g2_enter_context() {
        let mut registry = IdentityRegistry::new();
        let owner_g1 = claim(&mut registry, 1, "Owner");
        let visitor = claim(&mut registry, 2, "Visitor");
        let newcomer = claim(&mut registry, 3, "Newcomer");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&visitor), owner(&owner_g1)).unwrap();
        let owner_g2 = replacement_identity(&mut registry, 1, 4, "Owner");
        let revision = hub.revision();

        assert!(matches!(
            hub.enter(&participant(&newcomer), &owner(&owner_g2)),
            Err(MyRoomHubError::StaleGeneration { .. })
        ));
        assert_eq!(hub.revision(), revision);
        assert_eq!(hub.member_count(), 1);
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn identity_advance_atomically_updates_owned_and_visited_roles() {
        let mut registry = IdentityRegistry::new();
        let owner_g1 = claim(&mut registry, 1, "Owner");
        let visited_owner = claim(&mut registry, 2, "VisitedOwner");
        let visitor = claim(&mut registry, 3, "Visitor");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&visitor), owner(&owner_g1)).unwrap();
        enter(&mut hub, participant(&visited_owner), owner(&visited_owner)).unwrap();
        enter(&mut hub, participant(&owner_g1), owner(&visited_owner)).unwrap();
        let owner_g2 = replacement_identity(&mut registry, 1, 4, "Owner");

        let transition = hub
            .advance_identity(&owner_g1, participant_with_rp(&owner_g2, 777))
            .unwrap();
        let IdentityAdvanceEffects { publications, .. } = transition.outcome();
        assert_eq!(publications.len(), 2);
        assert_eq!(publications[0].owner, visited_owner.user_no);
        assert_eq!(publications[1].owner, owner_g1.user_no);
        assert_eq!(
            hub.membership_owner(&owner_g1).unwrap(),
            visited_owner.user_no
        );
        assert!(matches!(
            hub.membership_owner(&owner_g2),
            Err(MyRoomHubError::StaleGeneration { .. })
        ));

        let _ = transition.commit(&mut hub).unwrap();
        assert_eq!(
            hub.membership_owner(&owner_g2).unwrap(),
            visited_owner.user_no
        );
        assert!(matches!(
            hub.membership_owner(&owner_g1),
            Err(MyRoomHubError::StaleGeneration { .. })
        ));
        let owned = hub.first_snapshot(&visitor).unwrap();
        assert!(matches!(
            &owned.slots[0],
            MyRoomSlot::Player(player) if player.rp == 777
        ));
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn migration_advance_preserves_distinct_owned_and_visited_presentations() {
        let mut registry = IdentityRegistry::new();
        let visited_owner = claim(&mut registry, 1, "VisitedOwner");
        let migrating_g1 = claim(&mut registry, 2, "Migrating");
        let owned_visitor = claim(&mut registry, 3, "OwnedVisitor");
        let mut hub = MyRoomHub::new();

        enter(
            &mut hub,
            participant_with_rp(&migrating_g1, 100),
            owner_with_state(&migrating_g1, 100, 20),
        )
        .unwrap();
        enter(
            &mut hub,
            participant(&owned_visitor),
            owner_with_state(&migrating_g1, 999, 999),
        )
        .unwrap();
        enter(&mut hub, participant(&visited_owner), owner(&visited_owner)).unwrap();
        enter(
            &mut hub,
            participant_with_rp(&migrating_g1, 200),
            owner(&visited_owner),
        )
        .unwrap();

        let mut migrating_g2 = replacement_identity(&mut registry, 2, 4, "Migrating");
        migrating_g2.source_ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 44));
        let transition = hub
            .advance_migrated_identity_if_tracked(&migrating_g1, &migrating_g2)
            .unwrap()
            .unwrap();
        let effects = transition.commit(&mut hub).unwrap();
        assert_eq!(effects.publications.len(), 2);

        let visited = hub.first_snapshot(&migrating_g2).unwrap();
        let MyRoomSlot::Player(visited_presentation) = &visited.slots[1] else {
            panic!("migrating visitor must remain in slot one");
        };
        assert_eq!(visited_presentation.rp, 200);
        assert_eq!(
            visited_presentation.p2p_address,
            Ipv4Addr::new(203, 0, 113, 44)
        );

        let owned = hub.first_snapshot(&owned_visitor).unwrap();
        let MyRoomSlot::Player(owned_presentation) = &owned.slots[0] else {
            panic!("migrating owner presentation must remain in slot zero");
        };
        assert_eq!(owned_presentation.rp, 100);
        assert_eq!(
            owned_presentation.p2p_address,
            Ipv4Addr::new(203, 0, 113, 44)
        );
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn explicit_participant_refresh_updates_only_canonical_roles() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let visitor = claim(&mut registry, 2, "Visitor");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&visitor), owner(&owner_id)).unwrap();
        let transition = hub
            .refresh_participant(&participant_with_rp(&owner_id, 555))
            .unwrap();
        let ParticipantRefreshEffects { publications } = transition.outcome();
        assert_eq!(publications.len(), 1);
        let _ = transition.commit(&mut hub).unwrap();
        let snapshot = hub.first_snapshot(&visitor).unwrap();
        assert!(matches!(
            &snapshot.slots[0],
            MyRoomSlot::Player(player) if player.rp == 555
        ));
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn owner_can_present_in_owned_room_while_visiting_elsewhere() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let other_owner = claim(&mut registry, 2, "OtherOwner");
        let visitor = claim(&mut registry, 3, "Visitor");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&owner_id), owner(&owner_id)).unwrap();
        enter(&mut hub, participant(&visitor), owner(&owner_id)).unwrap();
        let EnterOutcome::Moved {
            previous: Some(RoomEffect::Updated(previous)),
            ..
        } = enter(&mut hub, participant(&owner_id), owner(&other_owner)).unwrap()
        else {
            panic!("owner move should retain its occupied owned room");
        };
        assert_eq!(
            user_numbers(&previous.audience),
            vec![visitor.user_no.get()]
        );
        assert!(matches!(&previous.snapshot.slots[0], MyRoomSlot::Player(_)));
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn owner_disconnect_closes_owned_room_and_ejects_visitors() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let first = claim(&mut registry, 2, "First");
        let second = claim(&mut registry, 3, "Second");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&owner_id), owner(&owner_id)).unwrap();
        enter(&mut hub, participant(&first), owner(&owner_id)).unwrap();
        enter(&mut hub, participant(&second), owner(&owner_id)).unwrap();
        let MyRoomDisconnectOutcome::Applied(effects) = disconnect(&mut hub, &owner_id).unwrap()
        else {
            panic!("owner disconnect must apply");
        };
        assert_eq!(
            user_numbers(&effects.closed_owned_room.unwrap().ejected),
            vec![first.user_no.get(), second.user_no.get()]
        );
        assert_eq!(hub.room_count(), 0);
        assert_eq!(hub.member_count(), 0);
        assert!(hub.room_info(owner_id.user_no).is_none());
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn disconnect_reaches_exact_two_room_eight_membership_nine_generation_bounds() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let visited_owner = claim(&mut registry, 2, "VisitedOwner");
        let mut hub = MyRoomHub::new();

        for index in 0..VISITOR_CAPACITY {
            let visitor = claim(
                &mut registry,
                u64::try_from(index + 10).unwrap(),
                &format!("OwnedVisitor{index}"),
            );
            enter(&mut hub, participant(&visitor), owner(&owner_id)).unwrap();
        }
        enter(&mut hub, participant(&owner_id), owner(&visited_owner)).unwrap();
        assert_eq!(hub.room_count(), MAX_TRANSITION_ROOMS);
        assert_eq!(hub.member_count(), MAX_TRANSITION_MEMBERSHIPS);
        assert_eq!(hub.generations.len(), MAX_TRANSITION_GENERATIONS);

        let transition = hub.disconnect(&owner_id).unwrap();
        assert_eq!(transition.rooms.len(), MAX_TRANSITION_ROOMS);
        assert_eq!(transition.memberships.len(), MAX_TRANSITION_MEMBERSHIPS);
        assert_eq!(transition.generations.len(), MAX_TRANSITION_GENERATIONS);
        assert_eq!(hub.room_count(), MAX_TRANSITION_ROOMS);
        assert_eq!(hub.member_count(), MAX_TRANSITION_MEMBERSHIPS);

        let MyRoomDisconnectOutcome::Applied(effects) = transition.commit(&mut hub).unwrap() else {
            panic!("tracked owner disconnect must apply");
        };
        assert_eq!(
            effects.closed_owned_room.unwrap().ejected.len(),
            VISITOR_CAPACITY
        );
        assert!(matches!(
            effects.membership_room,
            Some(RoomEffect::Deleted { owner }) if owner == visited_owner.user_no
        ));
        assert_eq!(hub.room_count(), 0);
        assert_eq!(hub.member_count(), 0);
        assert!(hub.generations.is_empty());
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn transition_bound_rejects_one_over_without_mutation() {
        // A valid disconnect can touch only its membership room and owned room,
        // delete itself plus seven owned-room visitors, and prune those eight
        // identities plus the now-empty membership-room owner. Therefore the
        // 2/8/9 case above is the structural maximum; these synthetic deltas
        // exercise every typed guard immediately beyond its reachable bound.
        let mut registry = IdentityRegistry::new();
        let identity = claim(&mut registry, 1, "Guard");
        let hub = MyRoomHub::new();

        let room = RoomState::new(owner(&identity));
        let rooms = (0..=MAX_TRANSITION_ROOMS)
            .map(|_| RoomChange {
                owner: OwnerKey(identity.user_no),
                value: Some(room.clone()),
            })
            .collect();
        assert!(matches!(
            hub.transition(MyRoomRevision(1), rooms, Vec::new(), Vec::new(), ()),
            Err(MyRoomHubError::TransitionTooLarge {
                rooms,
                memberships: 0,
                generations: 0,
            }) if rooms == MAX_TRANSITION_ROOMS + 1
        ));

        let memberships = (0..=MAX_TRANSITION_MEMBERSHIPS)
            .map(|_| MembershipChange {
                member: identity.user_no,
                value: None,
            })
            .collect();
        assert!(matches!(
            hub.transition(
                MyRoomRevision(1),
                Vec::new(),
                memberships,
                Vec::new(),
                (),
            ),
            Err(MyRoomHubError::TransitionTooLarge {
                rooms: 0,
                memberships,
                generations: 0,
            }) if memberships == MAX_TRANSITION_MEMBERSHIPS + 1
        ));

        let generations = (0..=MAX_TRANSITION_GENERATIONS)
            .map(|_| GenerationChange {
                user_no: identity.user_no,
                value: Some(identity.clone()),
            })
            .collect();

        assert!(matches!(
            hub.transition(MyRoomRevision(1), Vec::new(), Vec::new(), generations, ()),
            Err(MyRoomHubError::TransitionTooLarge {
                rooms: 0,
                memberships: 0,
                generations,
            }) if generations == MAX_TRANSITION_GENERATIONS + 1
        ));
        assert_eq!(hub.revision(), MyRoomRevision::default());
        assert_eq!(hub.room_count(), 0);
        assert_eq!(hub.member_count(), 0);
        assert!(hub.generations.is_empty());
    }

    #[test]
    fn stale_generation_disconnect_is_rejected_without_closing_room() {
        let mut registry = IdentityRegistry::new();
        let owner_g1 = claim(&mut registry, 1, "Owner");
        let visitor = claim(&mut registry, 2, "Visitor");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&visitor), owner(&owner_g1)).unwrap();
        let owner_g2 = replacement_identity(&mut registry, 1, 3, "Owner");
        let transition = hub
            .advance_identity(&owner_g1, participant(&owner_g2))
            .unwrap();
        let _ = transition.commit(&mut hub).unwrap();

        assert!(matches!(
            hub.disconnect(&owner_g1),
            Err(MyRoomHubError::StaleGeneration { .. })
        ));
        assert_eq!(hub.membership_owner(&visitor).unwrap(), owner_g2.user_no);
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn nonmember_queries_hide_only_genuinely_untracked_identities() {
        let mut registry = IdentityRegistry::new();
        let identity = claim(&mut registry, 1, "Untracked");
        let mut hub = MyRoomHub::new();
        assert_eq!(hub.membership_if_member(&identity).unwrap(), None);
        assert!(matches!(
            hub.disconnect_released(&released(&identity))
                .unwrap()
                .outcome(),
            MyRoomDisconnectOutcome::NotTracked
        ));

        enter(&mut hub, participant(&identity), owner(&identity)).unwrap();
        hub.memberships.clear();
        hub.rooms.clear();
        assert!(matches!(
            hub.membership_if_member(&identity),
            Err(MyRoomHubError::Invariant(
                MyRoomInvariantViolation::OrphanCanonicalGeneration { .. }
            ))
        ));
        assert!(matches!(
            hub.disconnect_released(&released(&identity)),
            Err(MyRoomHubError::Invariant(
                MyRoomInvariantViolation::OrphanCanonicalGeneration { .. }
            ))
        ));
    }

    #[test]
    fn role_without_canonical_generation_is_never_treated_as_untracked() {
        let mut registry = IdentityRegistry::new();
        let identity = claim(&mut registry, 1, "MissingCanonical");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&identity), owner(&identity)).unwrap();
        hub.generations.clear();

        assert!(matches!(
            hub.membership_if_member(&identity),
            Err(MyRoomHubError::Invariant(
                MyRoomInvariantViolation::MissingCanonicalGeneration { .. }
            ))
        ));
        assert!(matches!(
            hub.disconnect_released(&released(&identity)),
            Err(MyRoomHubError::Invariant(
                MyRoomInvariantViolation::MissingCanonicalGeneration { .. }
            ))
        ));
    }

    #[test]
    fn peer_audience_excludes_sender_in_slot_order() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let first = claim(&mut registry, 2, "First");
        let second = claim(&mut registry, 3, "Second");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&owner_id), owner(&owner_id)).unwrap();
        enter(&mut hub, participant(&first), owner(&owner_id)).unwrap();
        enter(&mut hub, participant(&second), owner(&owner_id)).unwrap();
        let audience = hub.peer_audience(&first).unwrap();
        assert_eq!(audience.sender_slot.get(), 1);
        assert_eq!(
            user_numbers(&audience.peers),
            vec![owner_id.user_no.get(), second.user_no.get()]
        );
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn owner_info_update_requires_present_exact_owner() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let visitor = claim(&mut registry, 2, "Visitor");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&owner_id), owner(&owner_id)).unwrap();
        enter(&mut hub, participant(&visitor), owner(&owner_id)).unwrap();
        let updated = MyRoomInfo {
            room_id: 99,
            ..MyRoomInfo::default()
        };
        let transition = hub.update_owner_info(&owner_id, updated.clone()).unwrap();
        let _ = transition.commit(&mut hub).unwrap();
        assert_eq!(hub.room_info(owner_id.user_no), Some(&updated));
        assert!(matches!(
            hub.update_owner_info(&visitor, MyRoomInfo::default()),
            Err(MyRoomHubError::NotPresentOwner { .. })
        ));
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn wire_invalid_participants_and_info_are_rejected_without_mutation() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&owner_id), owner(&owner_id)).unwrap();
        let revision = hub.revision();
        let snapshot = hub.first_snapshot(&owner_id).unwrap();
        let info = hub.room_info(owner_id.user_no).unwrap().clone();

        let mut oversized_club = participant(&owner_id).player().clone();
        oversized_club.club_name = "x".repeat(MAX_CLUB_NAME_UTF16_UNITS + 1);
        assert!(matches!(
            MyRoomParticipant::new(owner_id.clone(), oversized_club),
            Err(MyRoomHubError::Wire(MyRoomProtocolError::StringTooLong {
                field: "MyRoom club name",
                ..
            }))
        ));

        let mut oversized_identity = owner_id.clone();
        oversized_identity.nickname = "x".repeat(MAX_RIDER_NICKNAME_UTF16_UNITS + 1);
        let mut oversized_nickname = participant(&owner_id).player().clone();
        oversized_nickname.nickname = oversized_identity.nickname.clone();
        assert!(matches!(
            MyRoomParticipant::new(oversized_identity, oversized_nickname),
            Err(MyRoomHubError::Wire(MyRoomProtocolError::StringTooLong {
                field: "MyRoom rider nickname",
                ..
            }))
        ));

        let oversized_info = MyRoomInfo {
            room_password: "x".repeat(MAX_MYROOM_PASSWORD_UTF16_UNITS + 1),
            ..MyRoomInfo::default()
        };
        assert!(matches!(
            MyRoomOwner::new(participant(&owner_id), oversized_info.clone()),
            Err(MyRoomHubError::Wire(MyRoomProtocolError::StringTooLong {
                field: "MyRoom room password",
                ..
            }))
        ));
        assert!(matches!(
            hub.update_owner_info(&owner_id, oversized_info),
            Err(MyRoomHubError::Wire(MyRoomProtocolError::StringTooLong {
                field: "MyRoom room password",
                ..
            }))
        ));

        assert_eq!(hub.revision(), revision);
        assert_eq!(hub.first_snapshot(&owner_id).unwrap(), snapshot);
        assert_eq!(hub.room_info(owner_id.user_no), Some(&info));
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn invariant_audit_rechecks_wire_bounds() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&owner_id), owner(&owner_id)).unwrap();

        let room = hub.rooms.get_mut(&OwnerKey(owner_id.user_no)).unwrap();
        room.info.item_password = "x".repeat(MAX_MYROOM_PASSWORD_UTF16_UNITS + 1);
        assert_eq!(
            hub.audit_invariants(),
            Err(MyRoomInvariantViolation::InvalidRoomInfo {
                owner: owner_id.user_no
            })
        );

        let room = hub.rooms.get_mut(&OwnerKey(owner_id.user_no)).unwrap();
        room.info = MyRoomInfo::default();
        room.owner.player.club_name = "x".repeat(MAX_CLUB_NAME_UTF16_UNITS + 1);
        assert_eq!(
            hub.audit_invariants(),
            Err(MyRoomInvariantViolation::InvalidPlayerSnapshot {
                owner: owner_id.user_no,
                slot: 0
            })
        );
    }

    #[test]
    fn deleting_last_member_deletes_room_and_prunes_generations() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&owner_id), owner(&owner_id)).unwrap();
        assert_eq!(
            leave(&mut hub, &owner_id).unwrap().room,
            RoomEffect::Deleted {
                owner: owner_id.user_no
            }
        );
        assert_eq!(hub.room_count(), 0);
        assert_eq!(hub.member_count(), 0);
        assert!(hub.generations.is_empty());
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn transition_is_bounded_and_does_not_mutate_until_commit() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let mut hub = MyRoomHub::new();
        let transition = hub
            .enter(&participant(&owner_id), &owner(&owner_id))
            .unwrap();
        assert_eq!(transition.rooms.len(), 1);
        assert_eq!(transition.memberships.len(), 1);
        assert_eq!(transition.generations.len(), 1);
        assert_eq!(hub.room_count(), 0);
        let _ = transition.commit(&mut hub).unwrap();
        assert_eq!(hub.room_count(), 1);
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn stale_transition_commit_leaves_live_state_unchanged() {
        let mut registry = IdentityRegistry::new();
        let first = claim(&mut registry, 1, "First");
        let second = claim(&mut registry, 2, "Second");
        let hub = MyRoomHub::new();
        let first_plan = hub.enter(&participant(&first), &owner(&first)).unwrap();
        let second_plan = hub.enter(&participant(&second), &owner(&second)).unwrap();
        let mut live = hub;
        let _ = first_plan.commit(&mut live).unwrap();
        let revision = live.revision();
        let rooms = live.room_count();

        assert_eq!(
            second_plan.commit(&mut live),
            Err(MyRoomCommitError::StaleRevision {
                planned: 0,
                current: 1,
            })
        );
        assert_eq!(live.revision(), revision);
        assert_eq!(live.room_count(), rooms);
        assert_eq!(live.membership_owner(&first).unwrap(), first.user_no);
        live.audit_invariants().unwrap();
    }

    #[test]
    fn revision_exhaustion_is_typed_and_allocation_free() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let mut hub = MyRoomHub::new();
        hub.revision = MyRoomRevision(u64::MAX);
        assert!(matches!(
            hub.enter(&participant(&owner_id), &owner(&owner_id)),
            Err(MyRoomHubError::RevisionExhausted)
        ));
        assert_eq!(hub.room_count(), 0);
        assert_eq!(hub.member_count(), 0);
    }

    #[test]
    fn invariant_audit_detects_generation_disagreement() {
        let mut registry = IdentityRegistry::new();
        let owner_g1 = claim(&mut registry, 1, "Owner");
        let visitor = claim(&mut registry, 2, "Visitor");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&visitor), owner(&owner_g1)).unwrap();
        let owner_g2 = replacement_identity(&mut registry, 1, 3, "Owner");
        hub.generations.insert(owner_g1.user_no, owner_g2.clone());
        assert_eq!(
            hub.audit_invariants(),
            Err(MyRoomInvariantViolation::CanonicalGenerationMismatch {
                user_no: owner_g1.user_no,
                expected: owner_g2.generation.get(),
                actual: owner_g1.generation.get(),
            })
        );
    }

    #[test]
    fn participant_constructor_rejects_mismatched_snapshot() {
        let mut registry = IdentityRegistry::new();
        let identity = claim(&mut registry, 1, "Rider");
        let mut player = participant(&identity).player().clone();
        player.user_no += 1;
        assert!(matches!(
            MyRoomParticipant::new(identity, player),
            Err(MyRoomHubError::SnapshotUserNoMismatch { .. })
        ));
    }

    #[test]
    fn visitor_refresh_cannot_overwrite_a_different_user_in_the_mapped_slot() {
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let visitor = claim(&mut registry, 2, "Visitor");
        let intruder = claim(&mut registry, 3, "Intruder");
        let mut room = RoomState::new(owner(&owner_id));
        let slot = MyRoomSlotIndex(1);
        room.add_member(slot, participant(&visitor)).unwrap();
        let before = room.participant(slot).unwrap().clone();

        assert_eq!(
            room.replace_participant(slot, participant(&intruder)),
            Err(MyRoomInvariantViolation::MembershipSlotMismatch {
                member: intruder.user_no,
                slot: slot.get(),
            })
        );
        assert_eq!(room.participant(slot), Some(&before));
    }

    #[test]
    fn exact_capacity_move_accounts_for_old_owner_prune_before_rejecting() {
        let mut registry = IdentityRegistry::new();
        let old_owner = claim(&mut registry, 1, "OldOwner");
        let mover = claim(&mut registry, 2, "Mover");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&mover), owner(&old_owner)).unwrap();
        fill_with_owned_rooms(&mut hub, &mut registry, 10, MAX_MYROOM_IDENTITIES - 2);
        assert_eq!(hub.generations.len(), MAX_MYROOM_IDENTITIES);

        let destination = claim(&mut registry, 1_000, "FreshDestination");
        let transition = hub
            .enter(&participant(&mover), &owner(&destination))
            .unwrap();
        assert_eq!(transition.generations.len(), 2);
        assert_eq!(hub.membership_owner(&mover).unwrap(), old_owner.user_no);
        let _ = transition.commit(&mut hub).unwrap();

        assert_eq!(hub.generations.len(), MAX_MYROOM_IDENTITIES);
        assert!(!hub.generations.contains_key(&old_owner.user_no));
        assert!(hub.generations.contains_key(&destination.user_no));
        assert_eq!(hub.membership_owner(&mover).unwrap(), destination.user_no);
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn exact_capacity_move_rolls_back_when_old_owner_remains_referenced() {
        let mut registry = IdentityRegistry::new();
        let old_owner = claim(&mut registry, 1, "OldOwner");
        let mover = claim(&mut registry, 2, "Mover");
        let other_owner = claim(&mut registry, 3, "OtherOwner");
        let mut hub = MyRoomHub::new();
        enter(&mut hub, participant(&mover), owner(&old_owner)).unwrap();
        enter(&mut hub, participant(&other_owner), owner(&other_owner)).unwrap();
        enter(&mut hub, participant(&old_owner), owner(&other_owner)).unwrap();
        fill_with_owned_rooms(&mut hub, &mut registry, 10, MAX_MYROOM_IDENTITIES - 3);
        assert_eq!(hub.generations.len(), MAX_MYROOM_IDENTITIES);

        let destination = claim(&mut registry, 1_000, "FreshDestination");
        let revision = hub.revision();
        let rooms = hub.room_count();
        let members = hub.member_count();
        assert!(matches!(
            hub.enter(&participant(&mover), &owner(&destination)),
            Err(MyRoomHubError::IdentityCapacity {
                maximum: MAX_MYROOM_IDENTITIES,
                requested,
            }) if requested == MAX_MYROOM_IDENTITIES + 1
        ));

        assert_eq!(hub.revision(), revision);
        assert_eq!(hub.room_count(), rooms);
        assert_eq!(hub.member_count(), members);
        assert_eq!(hub.generations.len(), MAX_MYROOM_IDENTITIES);
        assert_eq!(hub.membership_owner(&mover).unwrap(), old_owner.user_no);
        assert_eq!(
            hub.membership_owner(&old_owner).unwrap(),
            other_owner.user_no
        );
        hub.audit_invariants().unwrap();
    }

    #[test]
    fn identity_capacity_is_injected_from_runtime_admission() {
        assert_eq!(MAX_MYROOM_IDENTITIES, 256);
        let mut registry = IdentityRegistry::new();
        let owner_id = claim(&mut registry, 1, "Owner");
        let visitor = claim(&mut registry, 2, "Visitor");
        let mut hub =
            MyRoomHub::with_identity_capacity(NonZeroUsize::new(1).expect("one is nonzero"));
        enter(&mut hub, participant(&owner_id), owner(&owner_id)).unwrap();
        let revision = hub.revision();

        assert!(matches!(
            hub.enter(&participant(&visitor), &owner(&owner_id)),
            Err(MyRoomHubError::IdentityCapacity {
                maximum: 1,
                requested: 2,
            })
        ));
        assert_eq!(hub.revision(), revision);
        assert_eq!(hub.member_count(), 1);
        hub.audit_invariants().unwrap();
    }
}
