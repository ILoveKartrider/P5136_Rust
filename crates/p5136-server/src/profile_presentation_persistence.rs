//! Cancellation-independent persistence for profile presentation mutations.
//!
//! The profile lane retains the exact identity operation which admitted the
//! write.  A separately carried [`IdentityBinding`] lets the World actor fence
//! cache publication against that same generation.  Once submitted, the
//! blocking write and its pre-reserved completion capability no longer depend
//! on the requesting session future.

use std::fmt;

use p5136_profile::{Profile, ProfileMutation, ProfileStore, ProfileStoreError, SavedProfile};
use thiserror::Error;
use tokio::sync::oneshot;

use crate::{
    identity::IdentityBinding,
    myroom_persistence::{MyRoomCompletionSlot, MyRoomProfileCompletion},
    profile_durability::{ExactDurability, ExactDurabilityError, ExactProfileTransaction},
    profile_io::{ProfileIoCompletion, ProfileIoError, ProfileJobAdmission},
};

pub(crate) const PROFILE_PRESENTATION_WRITE_OPERATION: &str =
    "persist profile presentation mutation";

#[cfg(test)]
pub(crate) type ProfilePresentationPersistenceTestHook = std::sync::Arc<dyn Fn() + Send + Sync>;

/// One absolute profile-backed value which is safe to patch into actor-owned
/// room presentations after the matching immutable revision is durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfilePresentationMutation {
    /// Replaces the persisted port. Zero is an absolute clear. The client-
    /// reported address is deliberately absent, and the value is not a live
    /// endpoint capability until World publishes it for the same active
    /// identity generation.
    SetP2pPort(u16),
}

impl ProfilePresentationMutation {
    fn apply(self, current: &Profile) -> ProfileMutation<Self> {
        match self {
            Self::SetP2pPort(port) if current.rider.p2p_port == i32::from(port) => {
                ProfileMutation::Unchanged(self)
            }
            Self::SetP2pPort(port) => {
                let mut next = current.clone();
                next.rider.p2p_port = i32::from(port);
                ProfileMutation::changed(self, next)
            }
        }
    }

    fn is_applied_to(self, profile: &Profile) -> bool {
        match self {
            Self::SetP2pPort(port) => profile.rider.p2p_port == i32::from(port),
        }
    }
}

/// How an exact-generation World publication resolved after disk durability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfilePresentationPublication {
    /// The exact identity remains actively owned and its runtime caches were
    /// patched after durability.
    ActiveCachesUpdated,
    /// The exact generation is current but ownerless. The disk value remains,
    /// while every runtime endpoint cache stays revoked.
    PersistedWhileOwnerless,
    PersistedAfterSupersession,
    PersistedAfterRelease,
}

impl ProfilePresentationPublication {
    /// Only an actively owned exact generation may turn a durable endpoint
    /// report into a live runtime capability.
    pub(crate) const fn updates_runtime_caches(self) -> bool {
        matches!(self, Self::ActiveCachesUpdated)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProfilePresentationWriteReceipt {
    mutation: ProfilePresentationMutation,
    revision: u64,
    publication: ProfilePresentationPublication,
}

impl ProfilePresentationWriteReceipt {
    pub(crate) const fn mutation(&self) -> ProfilePresentationMutation {
        self.mutation
    }

    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) const fn publication(&self) -> ProfilePresentationPublication {
        self.publication
    }
}

#[derive(Debug, Error)]
pub(crate) enum ProfilePresentationPersistError {
    #[error(transparent)]
    Store(Box<ProfileStoreError>),

    #[error(
        "profile-presentation durability confirmation changed immutable receipt from {expected:?} to {actual:?}"
    )]
    DurabilityReceiptChanged {
        expected: Box<SavedProfile>,
        actual: Option<Box<SavedProfile>>,
    },

    #[error(
        "profile-presentation revision {revision} remained durability-uncertain: initial commit: {initial}; confirmation: {confirmation}"
    )]
    DurabilityUnconfirmed {
        revision: u64,
        initial: Box<ProfileStoreError>,
        #[source]
        confirmation: Box<ProfileStoreError>,
    },

    #[error("the durable profile does not contain submitted mutation {mutation:?}")]
    PersistedValueMismatch {
        mutation: ProfilePresentationMutation,
    },

    #[error("the durable profile-presentation transaction did not resolve an immutable revision")]
    MissingDurableRevision,
}

impl From<ProfileStoreError> for ProfilePresentationPersistError {
    fn from(source: ProfileStoreError) -> Self {
        Self::Store(Box::new(source))
    }
}

impl ExactDurabilityError for ProfilePresentationPersistError {
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
pub(crate) enum ProfilePresentationWriteError {
    #[error("the World actor stopped before the profile-presentation write completed")]
    WorldStopped,

    #[error(
        "the profile-presentation write used an identity operation minted by another World actor"
    )]
    ForeignIdentityOperation,

    #[error("the profile-to-World completion mailbox is closed")]
    CompletionMailboxClosed,

    #[error("the accepted profile-presentation write lost its completion capability")]
    AcceptedOutcomeLost,

    #[error("active profile-presentation request completed with {publication:?}")]
    UnexpectedPublication {
        publication: ProfilePresentationPublication,
    },

    #[error(transparent)]
    Persistence(#[from] ProfilePresentationPersistError),
}

#[derive(Debug)]
pub(crate) struct DurableProfilePresentationMutation {
    mutation: ProfilePresentationMutation,
    saved: SavedProfile,
}

impl DurableProfilePresentationMutation {
    pub(crate) const fn mutation(&self) -> ProfilePresentationMutation {
        self.mutation
    }

    #[cfg(test)]
    pub(crate) const fn revision(&self) -> u64 {
        self.saved.revision
    }

    pub(crate) fn into_receipt(
        self,
        publication: ProfilePresentationPublication,
    ) -> ProfilePresentationWriteReceipt {
        ProfilePresentationWriteReceipt {
            mutation: self.mutation,
            revision: self.saved.revision,
            publication,
        }
    }
}

pub(crate) type ProfilePresentationProfileJobResult = Result<
    ProfileIoCompletion<
        Result<DurableProfilePresentationMutation, ProfilePresentationPersistError>,
    >,
    ProfileIoError,
>;

pub(crate) type ProfilePresentationWriteReply =
    Result<ProfilePresentationWriteReceipt, ProfilePresentationWriteError>;

/// Terminal result delivered through the pre-reserved World completion slot.
///
/// Both variants retain the exact identity and request reply sender.  The
/// actor therefore remains responsible for publication and the terminal reply
/// even when the original receiver has already been cancelled.
#[derive(Debug)]
pub(crate) enum ProfilePresentationCompletion {
    AcceptedOutcomeLost {
        expected: Box<IdentityBinding>,
        requested: ProfilePresentationMutation,
        reply: oneshot::Sender<ProfilePresentationWriteReply>,
    },
    Finished {
        expected: Box<IdentityBinding>,
        requested: ProfilePresentationMutation,
        result: Box<ProfilePresentationProfileJobResult>,
        reply: oneshot::Sender<ProfilePresentationWriteReply>,
    },
}

#[must_use = "a prepared profile-presentation write must be submitted or dropped before mutation"]
pub(crate) struct PreparedProfilePresentationWrite {
    admission: ProfileJobAdmission,
    expected: Box<IdentityBinding>,
    mutation: ProfilePresentationMutation,
    completion: MyRoomCompletionSlot,
    reply: oneshot::Sender<ProfilePresentationWriteReply>,
    #[cfg(test)]
    test_hook: Option<ProfilePresentationPersistenceTestHook>,
}

impl fmt::Debug for PreparedProfilePresentationWrite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedProfilePresentationWrite")
            .field("admission", &self.admission)
            .field("expected", &self.expected)
            .field("mutation", &self.mutation)
            .field("completion", &self.completion)
            .finish_non_exhaustive()
    }
}

impl PreparedProfilePresentationWrite {
    pub(crate) fn new(
        admission: ProfileJobAdmission,
        expected: IdentityBinding,
        mutation: ProfilePresentationMutation,
        completion: MyRoomCompletionSlot,
    ) -> (Self, oneshot::Receiver<ProfilePresentationWriteReply>) {
        let (reply, response) = oneshot::channel();
        (
            Self {
                admission,
                expected: Box::new(expected),
                mutation,
                completion,
                reply,
                #[cfg(test)]
                test_hook: None,
            },
            response,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_test_hook(
        mut self,
        test_hook: ProfilePresentationPersistenceTestHook,
    ) -> Self {
        self.test_hook = Some(test_hook);
        self
    }

    #[cfg(test)]
    pub(crate) fn admitted_nickname(&self) -> &str {
        self.admission.subject().nickname()
    }

    #[cfg(test)]
    pub(crate) fn expected_identity(&self) -> &IdentityBinding {
        &self.expected
    }

    #[cfg(test)]
    pub(crate) const fn mutation(&self) -> ProfilePresentationMutation {
        self.mutation
    }

    pub(crate) fn submit(self) {
        let Self {
            admission,
            expected,
            mutation,
            completion,
            reply,
            #[cfg(test)]
            test_hook,
        } = self;
        let accepted = AcceptedCompletionGuard {
            terminal: Some(AcceptedCompletion {
                completion,
                expected,
                requested: mutation,
                reply,
            }),
        };
        admission.submit_with_completion(
            PROFILE_PRESENTATION_WRITE_OPERATION,
            move |store, _, subject| {
                #[cfg(test)]
                if let Some(test_hook) = test_hook {
                    test_hook();
                }
                persist_profile_presentation_mutation(store, subject.nickname(), mutation)
            },
            move |result| accepted.finish(result),
        );
    }
}

struct AcceptedCompletion {
    completion: MyRoomCompletionSlot,
    expected: Box<IdentityBinding>,
    requested: ProfilePresentationMutation,
    reply: oneshot::Sender<ProfilePresentationWriteReply>,
}

struct AcceptedCompletionGuard {
    terminal: Option<AcceptedCompletion>,
}

impl AcceptedCompletionGuard {
    fn finish(mut self, result: ProfilePresentationProfileJobResult) {
        let terminal = self
            .terminal
            .take()
            .expect("the profile-presentation completion guard resolves exactly once");
        terminal
            .completion
            .send(MyRoomProfileCompletion::ProfilePresentation(
                ProfilePresentationCompletion::Finished {
                    expected: terminal.expected,
                    requested: terminal.requested,
                    result: Box::new(result),
                    reply: terminal.reply,
                },
            ));
    }
}

impl Drop for AcceptedCompletionGuard {
    fn drop(&mut self) {
        if let Some(terminal) = self.terminal.take() {
            terminal
                .completion
                .send(MyRoomProfileCompletion::ProfilePresentation(
                    ProfilePresentationCompletion::AcceptedOutcomeLost {
                        expected: terminal.expected,
                        requested: terminal.requested,
                        reply: terminal.reply,
                    },
                ));
        }
    }
}

fn persist_profile_presentation_mutation(
    store: &ProfileStore,
    nickname: &str,
    mutation: ProfilePresentationMutation,
) -> Result<DurableProfilePresentationMutation, ProfilePresentationPersistError> {
    let transaction = store.transaction(nickname, |current| mutation.apply(current))?;
    let (persisted, profile, durability) = ExactProfileTransaction::from(transaction).into_parts();
    if persisted != mutation || !mutation.is_applied_to(&profile) {
        return Err(ProfilePresentationPersistError::PersistedValueMismatch { mutation });
    }
    let saved = require_exact_revision(durability, store, nickname)?;
    Ok(DurableProfilePresentationMutation {
        mutation: persisted,
        saved,
    })
}

fn require_exact_revision(
    durability: ExactDurability,
    store: &ProfileStore,
    nickname: &str,
) -> Result<SavedProfile, ProfilePresentationPersistError> {
    durability
        .confirm_exact::<ProfilePresentationPersistError>(store, nickname)?
        .ok_or(ProfilePresentationPersistError::MissingDurableRevision)
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        net::{IpAddr, Ipv4Addr},
        num::NonZeroUsize,
        path::PathBuf,
        sync::{Arc, Barrier},
        time::Duration,
    };

    use p5136_profile::ProfileStore;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        SessionId,
        identity::IdentityRegistry,
        myroom_persistence::MyRoomCompletionBridge,
        profile_io::{ProfileIoBootstrap, ProfileIoLimits},
    };

    #[test]
    fn p2p_port_mutation_is_idempotent_and_zero_is_an_absolute_clear() {
        let directory = tempdir().unwrap();
        let store = ProfileStore::new(directory.path());
        let mutation = ProfilePresentationMutation::SetP2pPort(5_136);

        let first = persist_profile_presentation_mutation(&store, "Rider", mutation).unwrap();
        let second = persist_profile_presentation_mutation(&store, "rIDER", mutation).unwrap();
        assert_eq!(first.mutation(), mutation);
        assert_eq!(second.mutation(), mutation);
        assert_eq!(
            second.revision(),
            first.revision(),
            "an idempotent retry must reuse the exact immutable receipt"
        );
        assert_eq!(store.reload("Rider").unwrap().profile.rider.p2p_port, 5_136);

        let cleared = persist_profile_presentation_mutation(
            &store,
            "Rider",
            ProfilePresentationMutation::SetP2pPort(0),
        )
        .unwrap();
        assert!(cleared.revision() > second.revision());
        assert_eq!(store.reload("Rider").unwrap().profile.rider.p2p_port, 0);
    }

    #[test]
    fn only_active_exact_generation_publication_may_update_runtime_caches() {
        assert!(ProfilePresentationPublication::ActiveCachesUpdated.updates_runtime_caches());
        for persisted_only in [
            ProfilePresentationPublication::PersistedWhileOwnerless,
            ProfilePresentationPublication::PersistedAfterSupersession,
            ProfilePresentationPublication::PersistedAfterRelease,
        ] {
            assert!(!persisted_only.updates_runtime_caches());
        }
    }

    #[test]
    fn durability_confirmation_rejects_a_different_immutable_receipt() {
        let directory = tempdir().unwrap();
        let store = ProfileStore::new(directory.path());
        let durable = persist_profile_presentation_mutation(
            &store,
            "Rider",
            ProfilePresentationMutation::SetP2pPort(4_141),
        )
        .unwrap();
        let wrong_receipt = SavedProfile {
            nickname: durable.saved.nickname.clone(),
            revision: durable.saved.revision + 1,
            path: PathBuf::from("not-the-published-receipt.json"),
        };

        let error = require_exact_revision(
            ExactDurability::NeedsConfirmation {
                expected: wrong_receipt.clone(),
                initial: ProfileStoreError::CommittedButDurabilityUncertain {
                    nickname: wrong_receipt.nickname.clone(),
                    revision: wrong_receipt.revision,
                    path: wrong_receipt.path.clone(),
                    source: io::Error::other("synthetic initial durability warning"),
                },
            },
            &store,
            "Rider",
        )
        .unwrap_err();

        match error {
            ProfilePresentationPersistError::DurabilityReceiptChanged { expected, actual } => {
                assert_eq!(*expected, wrong_receipt);
                assert_eq!(
                    actual.map(|actual| actual.revision),
                    Some(durable.revision())
                );
            }
            other => panic!("expected exact-receipt mismatch, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn submitted_write_survives_request_receiver_cancellation_with_exact_identity() {
        let directory = tempdir().unwrap();
        let root = directory.path().to_owned();
        let bootstrap =
            ProfileIoBootstrap::acquire(root.clone(), ProfileIoLimits::for_tests(2, 2)).unwrap();
        let (profiles, runtime) = bootstrap.spawn();
        let session = SessionId::new(1);
        let mut identities = IdentityRegistry::new();
        let expected = identities
            .claim(session, IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), "Rider")
            .unwrap();
        let operation = identities.admit_operation(session).unwrap();
        let admission = profiles
            .admit(&expected.nickname, PROFILE_PRESENTATION_WRITE_OPERATION)
            .await
            .unwrap()
            .retain_identity_operation(operation);
        let (bridge, mut completions) = MyRoomCompletionBridge::channel(NonZeroUsize::MIN);
        let completion = bridge.reserve().await.unwrap();
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let entered_worker = Arc::clone(&entered);
        let release_worker = Arc::clone(&release);
        let mutation = ProfilePresentationMutation::SetP2pPort(4_000);
        let (prepared, response) = PreparedProfilePresentationWrite::new(
            admission,
            expected.clone(),
            mutation,
            completion,
        );
        assert_eq!(prepared.admitted_nickname(), expected.nickname);
        assert_eq!(prepared.expected_identity(), &expected);
        assert_eq!(prepared.mutation(), mutation);
        let prepared = prepared.with_test_hook(Arc::new(move || {
            entered_worker.wait();
            release_worker.wait();
        }));

        prepared.submit();
        entered.wait();
        drop(response);
        release.wait();

        let completion = tokio::time::timeout(Duration::from_secs(2), completions.recv())
            .await
            .unwrap()
            .unwrap();
        let MyRoomProfileCompletion::ProfilePresentation(ProfilePresentationCompletion::Finished {
            expected: completed_identity,
            requested,
            result,
            reply,
        }) = completion
        else {
            panic!("profile presentation write must use its dedicated completion variant");
        };
        assert_eq!(*completed_identity, expected);
        assert_eq!(requested, mutation);
        let completed = (*result).unwrap();
        let (durable, lane) = completed.into_parts();
        let durable = durable.unwrap();
        assert_eq!(durable.mutation(), mutation);
        let durable_revision = durable.revision();
        let receipt = durable.into_receipt(ProfilePresentationPublication::ActiveCachesUpdated);
        assert_eq!(receipt.mutation(), mutation);
        assert_eq!(receipt.revision(), durable_revision);
        assert_eq!(
            receipt.publication(),
            ProfilePresentationPublication::ActiveCachesUpdated
        );
        assert!(
            reply.send(Ok(receipt)).is_err(),
            "the cancelled request receiver must not own persistence or publication"
        );
        drop(lane);
        assert_eq!(
            ProfileStore::new(&root)
                .reload("Rider")
                .unwrap()
                .profile
                .rider
                .p2p_port,
            4_000
        );

        drop(profiles);
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn dropping_prepared_write_closes_reply_without_submission_or_completion() {
        let directory = tempdir().unwrap();
        let root = directory.path().to_owned();
        let bootstrap =
            ProfileIoBootstrap::acquire(root.clone(), ProfileIoLimits::for_tests(1, 1)).unwrap();
        let (profiles, runtime) = bootstrap.spawn();
        let session = SessionId::new(1);
        let mut identities = IdentityRegistry::new();
        let expected = identities
            .claim(session, IpAddr::V4(Ipv4Addr::LOCALHOST), "DropBeforeSubmit")
            .unwrap();
        let operation = identities.admit_operation(session).unwrap();
        let admission = profiles
            .admit(&expected.nickname, PROFILE_PRESENTATION_WRITE_OPERATION)
            .await
            .unwrap()
            .retain_identity_operation(operation);
        let (bridge, mut completions) = MyRoomCompletionBridge::channel(NonZeroUsize::MIN);
        let completion = bridge.reserve().await.unwrap();
        let (prepared, response) = PreparedProfilePresentationWrite::new(
            admission,
            expected,
            ProfilePresentationMutation::SetP2pPort(9_999),
            completion,
        );

        drop(prepared);
        assert!(response.await.is_err());
        assert!(matches!(
            completions.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert!(
            !ProfileStore::new(&root)
                .profile_exists("DropBeforeSubmit")
                .unwrap()
        );

        drop(profiles);
        runtime.shutdown().await.unwrap();
    }
}
