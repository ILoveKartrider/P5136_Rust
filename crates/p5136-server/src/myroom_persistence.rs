//! Cancellation-independent persistence capabilities for `MyRoom` owner info.
//!
//! The World actor registers one bounded ticket before a profile job is
//! submitted. A pre-reserved completion slot then guarantees that every
//! accepted blocking job can report exactly one terminal outcome without
//! awaiting or competing with the ordinary World command mailbox.

use std::{
    fmt,
    num::{NonZeroU64, NonZeroUsize},
};

use p5136_core::myroom_protocol::{MyRoomInfo, MyRoomProtocolError};
use p5136_profile::{ProfileMutation, ProfileStore, ProfileStoreError, SavedProfile};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use crate::{
    SessionId, WorldError,
    equipment_persistence::RiderEquipmentProfileCompletion,
    identity::{MigrationCompletion, MigrationPreflight, UserNo},
    main_emblem_persistence::MainEmblemProfileCompletion,
    myroom_hub::{MyRoomCommitError, MyRoomHubError},
    profile_durability::{ExactDurabilityError, ExactProfileTransaction},
    profile_io::{MyRoomProfileLease, ProfileIoCompletion, ProfileIoError, ProfileJobAdmission},
};

pub(crate) const MYROOM_INFO_WRITE_OPERATION: &str = "persist MyRoom owner info";

#[cfg(test)]
pub(crate) type MyRoomPersistenceTestHook = std::sync::Arc<dyn Fn() + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MyRoomProfileTicketId(NonZeroU64);

impl MyRoomProfileTicketId {
    pub(crate) const FIRST: Self = Self(NonZeroU64::MIN);

    #[must_use]
    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }

    #[must_use]
    pub(crate) const fn successor(self) -> Option<Self> {
        match self.0.get().checked_add(1) {
            Some(next) => match NonZeroU64::new(next) {
                Some(next) => Some(Self(next)),
                None => None,
            },
            None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MyRoomInfoPublication {
    ActiveOwnerEchoed,
    OwnerlessGenerationUpdated,
    PersistedAfterRelease,
    PersistedAfterSupersession,
    PersistedAfterRoleChange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MyRoomInfoWriteReceipt {
    info: MyRoomInfo,
    revision: u64,
    publication: MyRoomInfoPublication,
}

impl MyRoomInfoWriteReceipt {
    pub(crate) fn info(&self) -> &MyRoomInfo {
        &self.info
    }

    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) const fn publication(&self) -> MyRoomInfoPublication {
        self.publication
    }
}

#[derive(Debug, Error)]
pub(crate) enum MyRoomPersistError {
    #[error(transparent)]
    Store(Box<ProfileStoreError>),

    #[error(transparent)]
    Protocol(#[from] MyRoomProtocolError),

    #[error(
        "MyRoom durability confirmation changed immutable receipt from {expected:?} to {actual:?}"
    )]
    DurabilityReceiptChanged {
        expected: Box<SavedProfile>,
        actual: Option<Box<SavedProfile>>,
    },

    #[error(
        "MyRoom profile revision {revision} remained durability-uncertain: initial commit: {initial}; confirmation: {confirmation}"
    )]
    DurabilityUnconfirmed {
        revision: u64,
        initial: Box<ProfileStoreError>,
        #[source]
        confirmation: Box<ProfileStoreError>,
    },

    #[error("the durable MyRoom profile value differs from the submitted absolute value")]
    PersistedValueMismatch,

    #[error("the durable MyRoom transaction did not resolve an immutable profile revision")]
    MissingDurableRevision,
}

impl From<ProfileStoreError> for MyRoomPersistError {
    fn from(source: ProfileStoreError) -> Self {
        Self::Store(Box::new(source))
    }
}

impl ExactDurabilityError for MyRoomPersistError {
    fn durability_unconfirmed(
        revision: u64,
        initial: ProfileStoreError,
        confirmation: ProfileStoreError,
    ) -> Self {
        Self::DurabilityUnconfirmed {
            revision,
            initial: Box::new(initial),
            confirmation: Box::new(confirmation),
        }
    }

    fn durability_receipt_changed(expected: SavedProfile, actual: Option<SavedProfile>) -> Self {
        Self::DurabilityReceiptChanged {
            expected: Box::new(expected),
            actual: actual.map(Box::new),
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum MyRoomInfoWriteError {
    #[error("the World actor stopped before the MyRoom write completed")]
    WorldStopped,

    #[error("the MyRoom write used an identity operation minted by another World actor")]
    ForeignIdentityOperation,

    #[error("the MyRoom completion mailbox is closed")]
    CompletionMailboxClosed,

    #[error("the World is quiescing and no longer accepts MyRoom writes")]
    WorldQuiescing,

    #[error("session {session:?} is not authenticated for a MyRoom write")]
    UnauthenticatedSession { session: SessionId },

    #[error("profile admission for {admitted:?} does not match active MyRoom owner {active:?}")]
    ProfileSubjectMismatch { admitted: String, active: String },

    #[error("identity {user_no:?} is not a MyRoom member")]
    NotMember { user_no: UserNo },

    #[error("identity {user_no:?} is not the present owner of its MyRoom")]
    NotPresentOwner { user_no: UserNo },

    #[error("identity {user_no:?} already has a pending MyRoom owner-info write")]
    AlreadyPending { user_no: UserNo },

    #[error("MyRoom owner session {session:?} has no reservable outbound queue slot")]
    OutboundUnavailable { session: SessionId },

    #[error(transparent)]
    InvalidProposal(#[from] MyRoomProtocolError),

    #[error("the registered MyRoom write was abandoned before profile submission")]
    AbortedBeforeSubmission,

    #[error(transparent)]
    Persistence(#[from] MyRoomPersistError),
}

#[derive(Debug, Error)]
pub(crate) enum MyRoomPersistenceInvariantError {
    #[error("MyRoom profile ticket ID space is exhausted")]
    TicketIdExhausted,

    #[error("completion referenced unknown MyRoom profile ticket {ticket}")]
    UnknownTicket { ticket: u64 },

    #[error("accepted MyRoom profile ticket {ticket} lost its completion capability")]
    AcceptedOutcomeLost { ticket: u64 },

    #[error(
        "MyRoom profile ticket {ticket} expected subject {expected:?}, completed as {actual:?}"
    )]
    CompletionSubjectMismatch {
        ticket: u64,
        expected: String,
        actual: String,
    },

    #[error("MyRoom profile ticket {ticket} returned a different durable value")]
    DurableValueMismatch { ticket: u64 },

    #[error(
        "MyRoom pending-user index for {user_no:?} expected ticket {expected}, found {actual:?}"
    )]
    PendingIndexMismatch {
        user_no: UserNo,
        expected: u64,
        actual: Option<u64>,
    },

    #[error("MyRoom profile completion infrastructure failed for ticket {ticket}")]
    ProfileInfrastructure {
        ticket: u64,
        #[source]
        source: ProfileIoError,
    },

    #[error("MyRoom completion {operation} failed for ticket {ticket}")]
    Hub {
        ticket: u64,
        operation: &'static str,
        #[source]
        source: MyRoomHubError,
    },

    #[error("MyRoom completion commit failed for ticket {ticket}")]
    Commit {
        ticket: u64,
        #[source]
        source: MyRoomCommitError,
    },

    #[error(
        "MyRoom completion drain retained {pending} pending tickets and {indexed} user indexes"
    )]
    PendingAtDrain { pending: usize, indexed: usize },

    #[error("the MyRoom completion mailbox closed while the World actor was running")]
    CompletionMailboxClosed,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MyRoomCompletionDrainError {
    #[error("the MyRoom completion mailbox is closed")]
    MailboxClosed,

    #[error("the World actor stopped before the MyRoom completion barrier replied")]
    WorldStopped,

    #[error(
        "the MyRoom completion barrier retained {pending} pending tickets and {indexed} user indexes"
    )]
    Pending { pending: usize, indexed: usize },
}

#[derive(Debug)]
pub(crate) struct DurableMyRoomInfo {
    info: MyRoomInfo,
    revision: u64,
}

impl DurableMyRoomInfo {
    pub(crate) fn info(&self) -> &MyRoomInfo {
        &self.info
    }

    #[cfg(test)]
    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn into_receipt(self, publication: MyRoomInfoPublication) -> MyRoomInfoWriteReceipt {
        MyRoomInfoWriteReceipt {
            info: self.info,
            revision: self.revision,
            publication,
        }
    }
}

pub(crate) type MyRoomProfileJobResult =
    Result<ProfileIoCompletion<Result<DurableMyRoomInfo, MyRoomPersistError>>, ProfileIoError>;

/// Actor-consumable proof of the migration ACK policy.
///
/// Production completion always carries an ordered destination packet. Tests
/// that exercise the lower-level identity/sidecar transition may explicitly
/// omit transport publication without making that bypass representable in a
/// production build.
#[derive(Debug)]
pub(crate) enum MigrationAcknowledgement {
    Ordered(Vec<u8>),
    #[cfg(test)]
    Omitted,
}

#[derive(Debug)]
pub(crate) enum MigrationProfileCompletion {
    Aborted {
        preflight: Box<MigrationPreflight>,
    },
    Ready {
        preflight: Box<MigrationPreflight>,
        profile: Box<MyRoomProfileLease>,
        acknowledgement: MigrationAcknowledgement,
        reply: oneshot::Sender<Result<MigrationCompletion, WorldError>>,
    },
}

#[derive(Debug)]
pub(crate) enum MyRoomProfileCompletion {
    AbortedBeforeSubmission {
        ticket: MyRoomProfileTicketId,
    },
    AcceptedOutcomeLost {
        ticket: MyRoomProfileTicketId,
    },
    Finished {
        ticket: MyRoomProfileTicketId,
        result: Box<MyRoomProfileJobResult>,
    },
    RiderEquipment(RiderEquipmentProfileCompletion),
    MainEmblem(MainEmblemProfileCompletion),
    Migration(MigrationProfileCompletion),
    DrainBarrier {
        reply: oneshot::Sender<Result<(), MyRoomCompletionDrainError>>,
    },
}

#[derive(Clone)]
pub(crate) struct MyRoomCompletionBridge {
    sender: mpsc::Sender<MyRoomProfileCompletion>,
}

impl fmt::Debug for MyRoomCompletionBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MyRoomCompletionBridge")
            .field("maximum_capacity", &self.sender.max_capacity())
            .finish_non_exhaustive()
    }
}

impl MyRoomCompletionBridge {
    pub(crate) fn channel(
        capacity: NonZeroUsize,
    ) -> (Self, mpsc::Receiver<MyRoomProfileCompletion>) {
        let (sender, receiver) = mpsc::channel(capacity.get());
        (Self { sender }, receiver)
    }

    pub(crate) async fn reserve(&self) -> Result<MyRoomCompletionSlot, MyRoomInfoWriteError> {
        let permit = self
            .sender
            .clone()
            .reserve_owned()
            .await
            .map_err(|_| MyRoomInfoWriteError::CompletionMailboxClosed)?;
        Ok(MyRoomCompletionSlot { permit })
    }

    pub(crate) async fn drain_barrier(&self) -> Result<(), MyRoomCompletionDrainError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(MyRoomProfileCompletion::DrainBarrier { reply })
            .await
            .map_err(|_| MyRoomCompletionDrainError::MailboxClosed)?;
        response
            .await
            .map_err(|_| MyRoomCompletionDrainError::WorldStopped)?
    }
}

pub(crate) struct MyRoomCompletionSlot {
    permit: mpsc::OwnedPermit<MyRoomProfileCompletion>,
}

impl fmt::Debug for MyRoomCompletionSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MyRoomCompletionSlot")
            .finish_non_exhaustive()
    }
}

impl MyRoomCompletionSlot {
    pub(crate) fn send(self, completion: MyRoomProfileCompletion) {
        let _sender = self.permit.send(completion);
    }
}

pub(crate) struct PreparedMyRoomInfoWrite {
    admission: ProfileJobAdmission,
    proposed: MyRoomInfo,
    completion: MyRoomCompletionSlot,
    #[cfg(test)]
    test_hook: Option<MyRoomPersistenceTestHook>,
}

impl fmt::Debug for PreparedMyRoomInfoWrite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedMyRoomInfoWrite")
            .field("admission", &self.admission)
            .field("completion", &self.completion)
            .finish_non_exhaustive()
    }
}

impl PreparedMyRoomInfoWrite {
    pub(crate) fn new(
        admission: ProfileJobAdmission,
        proposed: MyRoomInfo,
        completion: MyRoomCompletionSlot,
    ) -> Self {
        Self {
            admission,
            proposed,
            completion,
            #[cfg(test)]
            test_hook: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_hook(mut self, test_hook: MyRoomPersistenceTestHook) -> Self {
        self.test_hook = Some(test_hook);
        self
    }

    pub(crate) fn admitted_nickname(&self) -> &str {
        self.admission.subject().nickname()
    }

    pub(crate) fn proposed(&self) -> &MyRoomInfo {
        &self.proposed
    }

    pub(crate) fn register(self, ticket: MyRoomProfileTicketId) -> RegisteredMyRoomInfoWrite {
        RegisteredMyRoomInfoWrite {
            admission: self.admission,
            proposed: self.proposed,
            abort: AbortBeforeSubmission::new(ticket, self.completion),
            #[cfg(test)]
            test_hook: self.test_hook,
        }
    }
}

#[must_use = "a registered MyRoom write must be submitted or explicitly aborted by drop"]
pub(crate) struct RegisteredMyRoomInfoWrite {
    admission: ProfileJobAdmission,
    proposed: MyRoomInfo,
    abort: AbortBeforeSubmission,
    #[cfg(test)]
    test_hook: Option<MyRoomPersistenceTestHook>,
}

struct AbortBeforeSubmission {
    ticket: MyRoomProfileTicketId,
    completion: Option<MyRoomCompletionSlot>,
}

impl fmt::Debug for RegisteredMyRoomInfoWrite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisteredMyRoomInfoWrite")
            .field("ticket", &self.abort.ticket)
            .field("admission", &self.admission)
            .field("armed", &self.abort.completion.is_some())
            .finish_non_exhaustive()
    }
}

impl RegisteredMyRoomInfoWrite {
    pub(crate) fn submit(self) {
        let Self {
            admission,
            proposed,
            abort,
            #[cfg(test)]
            test_hook,
        } = self;
        let (ticket, completion) = abort.disarm();
        let accepted = AcceptedCompletionGuard {
            ticket,
            completion: Some(completion),
        };
        admission.submit_with_completion(
            MYROOM_INFO_WRITE_OPERATION,
            move |store, _, subject| {
                #[cfg(test)]
                if let Some(test_hook) = test_hook {
                    test_hook();
                }
                persist_myroom_info(store, subject.nickname(), &proposed)
            },
            move |result| accepted.finish(result),
        );
    }
}

impl AbortBeforeSubmission {
    fn new(ticket: MyRoomProfileTicketId, completion: MyRoomCompletionSlot) -> Self {
        Self {
            ticket,
            completion: Some(completion),
        }
    }

    fn disarm(mut self) -> (MyRoomProfileTicketId, MyRoomCompletionSlot) {
        let completion = self
            .completion
            .take()
            .expect("the abort guard is disarmed exactly once");
        (self.ticket, completion)
    }
}

impl Drop for AbortBeforeSubmission {
    fn drop(&mut self) {
        if let Some(completion) = self.completion.take() {
            completion.send(MyRoomProfileCompletion::AbortedBeforeSubmission {
                ticket: self.ticket,
            });
        }
    }
}

struct AcceptedCompletionGuard {
    ticket: MyRoomProfileTicketId,
    completion: Option<MyRoomCompletionSlot>,
}

impl AcceptedCompletionGuard {
    fn finish(
        mut self,
        result: Result<
            ProfileIoCompletion<Result<DurableMyRoomInfo, MyRoomPersistError>>,
            ProfileIoError,
        >,
    ) {
        if let Some(completion) = self.completion.take() {
            completion.send(MyRoomProfileCompletion::Finished {
                ticket: self.ticket,
                result: Box::new(result),
            });
        }
    }
}

impl Drop for AcceptedCompletionGuard {
    fn drop(&mut self) {
        if let Some(completion) = self.completion.take() {
            completion.send(MyRoomProfileCompletion::AcceptedOutcomeLost {
                ticket: self.ticket,
            });
        }
    }
}

fn persist_myroom_info(
    store: &ProfileStore,
    nickname: &str,
    proposed: &MyRoomInfo,
) -> Result<DurableMyRoomInfo, MyRoomPersistError> {
    let transaction = store.transaction(nickname, |current| {
        if matches!(
            current.my_room.try_to_protocol_info(),
            Ok(ref current) if current == proposed
        ) {
            return ProfileMutation::Unchanged(Ok(()));
        }
        let mut profile = current.clone();
        match profile.my_room.try_apply_protocol_info(proposed) {
            Ok(()) => ProfileMutation::changed(Ok(()), profile),
            Err(error) => ProfileMutation::Unchanged(Err(error)),
        }
    })?;

    let (mutation, profile, durability) = ExactProfileTransaction::from(transaction).into_parts();
    mutation?;

    let persisted = profile.my_room.try_to_protocol_info()?;
    if &persisted != proposed {
        return Err(MyRoomPersistError::PersistedValueMismatch);
    }

    let confirmed = durability
        .confirm_exact::<MyRoomPersistError>(store, nickname)?
        .ok_or(MyRoomPersistError::MissingDurableRevision)?;

    Ok(DurableMyRoomInfo {
        info: persisted,
        revision: confirmed.revision,
    })
}

#[cfg(test)]
mod tests {
    use p5136_core::myroom_protocol::MyRoomInfo;
    use serde_json::json;
    use std::num::NonZeroUsize;

    use p5136_profile::{Profile, ProfileStore};

    use super::{
        AbortBeforeSubmission, MyRoomCompletionBridge, MyRoomInfoWriteError,
        MyRoomProfileCompletion, MyRoomProfileTicketId, persist_myroom_info,
    };

    #[test]
    fn absolute_persistence_is_idempotent_and_preserves_unknown_fields() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let mut profile = Profile::default();
        profile
            .my_room
            .extra
            .insert("FutureMyRoomField".to_owned(), json!([1, 2, 3]));
        let initial = store.save("DurableOwner", &profile).unwrap();
        let proposed = MyRoomInfo {
            room_id: 5136,
            bgm: 7,
            room_password: "room".to_owned(),
            item_password: "item".to_owned(),
            ..MyRoomInfo::default()
        };

        let first = persist_myroom_info(&store, "durableowner", &proposed).unwrap();
        assert_eq!(first.info(), &proposed);
        assert_eq!(first.revision(), initial.revision + 1);

        let second = persist_myroom_info(&store, "DURABLEOWNER", &proposed).unwrap();
        assert_eq!(second.info(), &proposed);
        assert_eq!(second.revision(), first.revision());
        let loaded = store.load_or_create("DurableOwner").unwrap();
        assert_eq!(
            loaded.profile.my_room.try_to_protocol_info().unwrap(),
            proposed
        );
        assert_eq!(
            loaded.profile.my_room.extra["FutureMyRoomField"],
            json!([1, 2, 3])
        );
    }

    #[tokio::test]
    async fn registered_capability_drop_reports_abort_without_awaiting() {
        let (bridge, mut receiver) = MyRoomCompletionBridge::channel(NonZeroUsize::MIN);
        let completion = bridge.reserve().await.unwrap();
        drop(AbortBeforeSubmission::new(
            MyRoomProfileTicketId::FIRST,
            completion,
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(MyRoomProfileCompletion::AbortedBeforeSubmission { ticket })
                if ticket == MyRoomProfileTicketId::FIRST
        ));
    }

    #[tokio::test]
    async fn completion_capacity_is_reserved_before_registration() {
        let (bridge, receiver) = MyRoomCompletionBridge::channel(NonZeroUsize::MIN);
        let first = bridge.reserve().await.unwrap();
        let mut second = Box::pin(bridge.reserve());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut second)
                .await
                .is_err()
        );
        drop(first);
        assert!(second.await.is_ok());

        drop(receiver);
        assert!(matches!(
            bridge.reserve().await,
            Err(MyRoomInfoWriteError::CompletionMailboxClosed)
        ));
    }
}
