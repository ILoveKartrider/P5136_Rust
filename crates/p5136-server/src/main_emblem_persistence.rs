//! Cancellation-independent persistence for the three P5136 main emblems.
//!
//! The stock client writes three signed 16-bit IDs.  A validated selection,
//! actor-owned ticket, canonical profile lane, and pre-reserved completion
//! capability keep malformed input, request cancellation, and slow storage
//! from producing an acknowledged-but-undurable update.

use std::fmt;

use p5136_core::myroom_protocol::UpdateMainEmblemRequest;
use p5136_profile::{
    EmblemCatalog, ProfileMutation, ProfileStore, ProfileStoreError, ProfileTransaction,
    SavedProfile,
};
use thiserror::Error;

use crate::{
    myroom_hub::MyRoomHubError,
    myroom_persistence::{MyRoomCompletionSlot, MyRoomProfileCompletion, MyRoomProfileTicketId},
    profile_io::{ProfileIoCompletion, ProfileIoError, ProfileJobAdmission},
};

pub(crate) const MAIN_EMBLEM_WRITE_OPERATION: &str = "persist main emblems";

#[cfg(test)]
pub(crate) type MainEmblemPersistenceTestHook = std::sync::Arc<dyn Fn() + Send + Sync>;

/// A complete main-emblem selection which has passed the configured
/// definition-catalog policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValidatedMainEmblemSelection([i16; 3]);

impl ValidatedMainEmblemSelection {
    pub(crate) fn validate(
        request: UpdateMainEmblemRequest,
        catalog: Option<&EmblemCatalog>,
    ) -> Result<Self, MainEmblemValidationError> {
        let values = [request.emblem_1, request.emblem_2, request.emblem_3];
        for (index, emblem) in values.into_iter().enumerate() {
            if emblem == 0 {
                continue;
            }
            let slot = u8::try_from(index + 1).expect("there are exactly three emblem slots");
            let catalog =
                catalog.ok_or(MainEmblemValidationError::CatalogUnavailable { slot, emblem })?;
            if emblem < 0 || !catalog.contains(emblem) {
                return Err(MainEmblemValidationError::UnknownEmblem { slot, emblem });
            }
        }
        Ok(Self(values))
    }

    pub(crate) const fn values(self) -> [i16; 3] {
        self.0
    }

    pub(crate) const fn wire_values(self) -> [u16; 3] {
        [
            u16::from_le_bytes(self.0[0].to_le_bytes()),
            u16::from_le_bytes(self.0[1].to_le_bytes()),
            u16::from_le_bytes(self.0[2].to_le_bytes()),
        ]
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MainEmblemValidationError {
    #[error("main-emblem slot {slot} selected {emblem}, but no definition catalog is configured")]
    CatalogUnavailable { slot: u8, emblem: i16 },

    #[error("main-emblem slot {slot} selected unknown emblem {emblem}")]
    UnknownEmblem { slot: u8, emblem: i16 },
}

#[derive(Debug, Error)]
pub(crate) enum MainEmblemPersistError {
    #[error(transparent)]
    Store(Box<ProfileStoreError>),

    #[error(
        "main-emblem durability confirmation changed immutable receipt from {expected:?} to {actual:?}"
    )]
    DurabilityReceiptChanged {
        expected: Box<SavedProfile>,
        actual: Option<Box<SavedProfile>>,
    },

    #[error(
        "main-emblem profile revision {revision} remained durability-uncertain: initial commit: {initial}; confirmation: {confirmation}"
    )]
    DurabilityUnconfirmed {
        revision: u64,
        initial: Box<ProfileStoreError>,
        #[source]
        confirmation: Box<ProfileStoreError>,
    },
}

impl From<ProfileStoreError> for MainEmblemPersistError {
    fn from(source: ProfileStoreError) -> Self {
        Self::Store(Box::new(source))
    }
}

#[derive(Debug, Error)]
pub(crate) enum MainEmblemWriteError {
    #[error("the World actor stopped before the main-emblem write completed")]
    WorldStopped,

    #[error("the main-emblem write used an identity operation minted by another World actor")]
    ForeignIdentityOperation,

    #[error("the profile-to-World completion mailbox is closed")]
    CompletionMailboxClosed,

    #[error("the World is quiescing and no longer accepts main-emblem writes")]
    WorldQuiescing,

    #[error("session {session:?} is not authenticated for a main-emblem write")]
    UnauthenticatedSession { session: crate::SessionId },

    #[error("profile admission for {admitted:?} does not match active rider identity {active:?}")]
    ProfileSubjectMismatch { admitted: String, active: String },

    #[error("identity {user_no:?} is not the present owner of its MyRoom")]
    NotPresentOwner { user_no: crate::UserNo },

    #[error("identity {user_no:?} already has a pending profile write")]
    AlreadyPending { user_no: crate::UserNo },

    #[error("the outbound queue for session {session:?} cannot reserve the success response")]
    OutboundUnavailable { session: crate::SessionId },

    #[error("the registered main-emblem write was abandoned before profile submission")]
    AbortedBeforeSubmission,

    #[error(transparent)]
    Persistence(#[from] MainEmblemPersistError),
}

impl MainEmblemWriteError {
    pub(crate) const fn is_request_rejection(&self) -> bool {
        matches!(
            self,
            Self::NotPresentOwner { .. }
                | Self::AlreadyPending { .. }
                | Self::OutboundUnavailable { .. }
                | Self::WorldQuiescing
        )
    }
}

#[derive(Debug, Error)]
pub(crate) enum MainEmblemPublicationInvariantError {
    #[error("main-emblem profile ticket ID space is exhausted")]
    TicketIdExhausted,

    #[error("completion referenced unknown main-emblem profile ticket {ticket}")]
    UnknownTicket { ticket: u64 },

    #[error("accepted main-emblem profile ticket {ticket} lost its completion capability")]
    AcceptedOutcomeLost { ticket: u64 },

    #[error(
        "main-emblem profile ticket {ticket} expected subject {expected:?}, completed as {actual:?}"
    )]
    CompletionSubjectMismatch {
        ticket: u64,
        expected: String,
        actual: String,
    },

    #[error("main-emblem profile ticket {ticket} returned a different durable selection")]
    DurableValueMismatch { ticket: u64 },

    #[error(
        "main-emblem pending-user index for {user_no:?} expected ticket {expected}, found {actual:?}"
    )]
    PendingIndexMismatch {
        user_no: crate::UserNo,
        expected: u64,
        actual: Option<u64>,
    },

    #[error("main-emblem profile completion infrastructure failed for ticket {ticket}")]
    ProfileInfrastructure {
        ticket: u64,
        #[source]
        source: ProfileIoError,
    },

    #[error("main-emblem MyRoom membership lookup failed for ticket {ticket}")]
    Hub {
        ticket: u64,
        #[source]
        source: MyRoomHubError,
    },

    #[error(
        "main-emblem protocol membership diverged for ticket {ticket}, room {room_id}, user {user_no:?}"
    )]
    ProtocolMembership {
        ticket: u64,
        room_id: u32,
        user_no: crate::UserNo,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MainEmblemPublication {
    ActiveOwnerCacheUpdated,
    PersistedAfterRoleChange,
    PersistedAfterSupersession,
    PersistedAfterRelease,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MainEmblemWriteReceipt {
    selection: ValidatedMainEmblemSelection,
    revision: Option<u64>,
    publication: MainEmblemPublication,
}

impl MainEmblemWriteReceipt {
    pub(crate) const fn selection(&self) -> ValidatedMainEmblemSelection {
        self.selection
    }

    pub(crate) const fn revision(&self) -> Option<u64> {
        self.revision
    }

    pub(crate) const fn publication(&self) -> MainEmblemPublication {
        self.publication
    }
}

#[derive(Debug)]
pub(crate) struct DurableMainEmblems {
    selection: ValidatedMainEmblemSelection,
    revision: Option<u64>,
}

impl DurableMainEmblems {
    pub(crate) const fn selection(&self) -> ValidatedMainEmblemSelection {
        self.selection
    }

    pub(crate) fn into_receipt(self, publication: MainEmblemPublication) -> MainEmblemWriteReceipt {
        MainEmblemWriteReceipt {
            selection: self.selection,
            revision: self.revision,
            publication,
        }
    }
}

pub(crate) type MainEmblemProfileJobResult =
    Result<ProfileIoCompletion<Result<DurableMainEmblems, MainEmblemPersistError>>, ProfileIoError>;

#[derive(Debug)]
pub(crate) enum MainEmblemProfileCompletion {
    AbortedBeforeSubmission {
        ticket: MyRoomProfileTicketId,
    },
    AcceptedOutcomeLost {
        ticket: MyRoomProfileTicketId,
    },
    Finished {
        ticket: MyRoomProfileTicketId,
        result: Box<MainEmblemProfileJobResult>,
    },
}

pub(crate) struct PreparedMainEmblemWrite {
    admission: ProfileJobAdmission,
    selection: ValidatedMainEmblemSelection,
    completion: MyRoomCompletionSlot,
    #[cfg(test)]
    test_hook: Option<MainEmblemPersistenceTestHook>,
}

impl fmt::Debug for PreparedMainEmblemWrite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedMainEmblemWrite")
            .field("admission", &self.admission)
            .field("selection", &self.selection)
            .field("completion", &self.completion)
            .finish_non_exhaustive()
    }
}

impl PreparedMainEmblemWrite {
    pub(crate) fn new(
        admission: ProfileJobAdmission,
        selection: ValidatedMainEmblemSelection,
        completion: MyRoomCompletionSlot,
    ) -> Self {
        Self {
            admission,
            selection,
            completion,
            #[cfg(test)]
            test_hook: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_hook(mut self, test_hook: MainEmblemPersistenceTestHook) -> Self {
        self.test_hook = Some(test_hook);
        self
    }

    pub(crate) fn admitted_nickname(&self) -> &str {
        self.admission.subject().nickname()
    }

    pub(crate) const fn selection(&self) -> ValidatedMainEmblemSelection {
        self.selection
    }

    pub(crate) fn register(self, ticket: MyRoomProfileTicketId) -> RegisteredMainEmblemWrite {
        RegisteredMainEmblemWrite {
            admission: self.admission,
            selection: self.selection,
            abort: AbortBeforeSubmission::new(ticket, self.completion),
            #[cfg(test)]
            test_hook: self.test_hook,
        }
    }
}

#[must_use = "a registered main-emblem write must be submitted or explicitly aborted by drop"]
pub(crate) struct RegisteredMainEmblemWrite {
    admission: ProfileJobAdmission,
    selection: ValidatedMainEmblemSelection,
    abort: AbortBeforeSubmission,
    #[cfg(test)]
    test_hook: Option<MainEmblemPersistenceTestHook>,
}

impl fmt::Debug for RegisteredMainEmblemWrite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisteredMainEmblemWrite")
            .field("ticket", &self.abort.ticket)
            .field("admission", &self.admission)
            .field("armed", &self.abort.completion.is_some())
            .finish_non_exhaustive()
    }
}

impl RegisteredMainEmblemWrite {
    pub(crate) fn submit(self) {
        let Self {
            admission,
            selection,
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
            MAIN_EMBLEM_WRITE_OPERATION,
            move |store, _, subject| {
                #[cfg(test)]
                if let Some(test_hook) = test_hook {
                    test_hook();
                }
                persist_main_emblems(store, subject.nickname(), selection)
            },
            move |result| accepted.finish(result),
        );
    }

    #[cfg(test)]
    pub(crate) fn inject_accepted_outcome_lost(self) {
        let Self {
            admission, abort, ..
        } = self;
        let (ticket, completion) = abort.disarm();
        drop(admission);
        drop(AcceptedCompletionGuard {
            ticket,
            completion: Some(completion),
        });
    }
}

struct AbortBeforeSubmission {
    ticket: MyRoomProfileTicketId,
    completion: Option<MyRoomCompletionSlot>,
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
            .expect("the main-emblem abort guard is disarmed exactly once");
        (self.ticket, completion)
    }
}

impl Drop for AbortBeforeSubmission {
    fn drop(&mut self) {
        if let Some(completion) = self.completion.take() {
            completion.send(MyRoomProfileCompletion::MainEmblem(
                MainEmblemProfileCompletion::AbortedBeforeSubmission {
                    ticket: self.ticket,
                },
            ));
        }
    }
}

struct AcceptedCompletionGuard {
    ticket: MyRoomProfileTicketId,
    completion: Option<MyRoomCompletionSlot>,
}

impl AcceptedCompletionGuard {
    fn finish(mut self, result: MainEmblemProfileJobResult) {
        if let Some(completion) = self.completion.take() {
            completion.send(MyRoomProfileCompletion::MainEmblem(
                MainEmblemProfileCompletion::Finished {
                    ticket: self.ticket,
                    result: Box::new(result),
                },
            ));
        }
    }
}

impl Drop for AcceptedCompletionGuard {
    fn drop(&mut self) {
        if let Some(completion) = self.completion.take() {
            completion.send(MyRoomProfileCompletion::MainEmblem(
                MainEmblemProfileCompletion::AcceptedOutcomeLost {
                    ticket: self.ticket,
                },
            ));
        }
    }
}

enum DurabilityEvidence {
    Confirmed(Option<SavedProfile>),
    NeedsConfirmation {
        expected: SavedProfile,
        initial: ProfileStoreError,
    },
}

fn persist_main_emblems(
    store: &ProfileStore,
    nickname: &str,
    selection: ValidatedMainEmblemSelection,
) -> Result<DurableMainEmblems, MainEmblemPersistError> {
    let values = selection.values();
    let transaction = store.transaction(nickname, |current| {
        if [
            current.rider.emblem1,
            current.rider.emblem2,
            current.rider.emblem3,
        ] == values
        {
            return ProfileMutation::Unchanged(());
        }
        let mut next = current.clone();
        next.rider.set_main_emblems(values[0], values[1], values[2]);
        ProfileMutation::changed((), next)
    })?;

    let durability = match transaction {
        ProfileTransaction::Unchanged { saved, .. } => DurabilityEvidence::Confirmed(saved),
        ProfileTransaction::Committed { saved, .. } => DurabilityEvidence::Confirmed(Some(saved)),
        ProfileTransaction::CommittedButDurabilityUncertain { saved, error, .. } => {
            DurabilityEvidence::NeedsConfirmation {
                expected: saved,
                initial: error,
            }
        }
    };
    let saved = match durability {
        DurabilityEvidence::Confirmed(saved) => saved,
        DurabilityEvidence::NeedsConfirmation { expected, initial } => {
            Some(confirm_exact_revision(store, nickname, &expected, initial)?)
        }
    };
    Ok(DurableMainEmblems {
        selection,
        revision: saved.map(|saved| saved.revision),
    })
}

fn confirm_exact_revision(
    store: &ProfileStore,
    nickname: &str,
    expected: &SavedProfile,
    initial: ProfileStoreError,
) -> Result<SavedProfile, MainEmblemPersistError> {
    let confirmed = store
        .confirm_latest_revision_durable(nickname)
        .map_err(
            |confirmation| MainEmblemPersistError::DurabilityUnconfirmed {
                revision: expected.revision,
                initial: Box::new(initial),
                confirmation: Box::new(confirmation),
            },
        )?;
    match confirmed {
        Some(actual) if &actual == expected => Ok(actual),
        actual => Err(MainEmblemPersistError::DurabilityReceiptChanged {
            expected: Box::new(expected.clone()),
            actual: actual.map(Box::new),
        }),
    }
}

#[cfg(test)]
mod tests {
    use p5136_core::myroom_protocol::UpdateMainEmblemRequest;
    use p5136_profile::ProfileStore;

    use super::{ValidatedMainEmblemSelection, persist_main_emblems};
    use crate::equipment_persistence::tests::catalog;

    #[test]
    fn validation_is_all_or_nothing_and_zero_is_an_explicit_sentinel() {
        let catalog = catalog();
        let valid = ValidatedMainEmblemSelection::validate(
            UpdateMainEmblemRequest {
                emblem_1: 7,
                emblem_2: 0,
                emblem_3: 9,
            },
            catalog.emblem_definitions(),
        )
        .unwrap();
        assert_eq!(valid.values(), [7, 0, 9]);

        assert!(
            ValidatedMainEmblemSelection::validate(
                UpdateMainEmblemRequest {
                    emblem_1: 7,
                    emblem_2: 10,
                    emblem_3: 9,
                },
                catalog.emblem_definitions(),
            )
            .is_err()
        );
        assert!(
            ValidatedMainEmblemSelection::validate(
                UpdateMainEmblemRequest {
                    emblem_1: 0,
                    emblem_2: 0,
                    emblem_3: 0,
                },
                None,
            )
            .is_ok()
        );
    }

    #[test]
    fn transaction_changes_only_three_emblem_fields_and_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(directory.path());
        let selection = ValidatedMainEmblemSelection::validate(
            UpdateMainEmblemRequest {
                emblem_1: 7,
                emblem_2: 8,
                emblem_3: 9,
            },
            catalog().emblem_definitions(),
        )
        .unwrap();

        let first = persist_main_emblems(&store, "Rider", selection).unwrap();
        let first_revision = first.revision.expect("a changed profile has a revision");
        let loaded = store.load_or_create("Rider").unwrap();
        assert_eq!(
            [
                loaded.profile.rider.emblem1,
                loaded.profile.rider.emblem2,
                loaded.profile.rider.emblem3,
            ],
            [7, 8, 9]
        );

        let second = persist_main_emblems(&store, "Rider", selection).unwrap();
        assert_eq!(second.revision, Some(first_revision));
    }
}
