//! Shared exact-revision confirmation for profile transactions.
//!
//! A committed-but-uncertain transaction may be acknowledged only after the
//! store confirms the same immutable receipt.  Keeping the comparison here
//! prevents individual profile writers from accidentally accepting a newer,
//! missing, or otherwise different revision as confirmation.

use p5136_profile::{Profile, ProfileStore, ProfileStoreError, ProfileTransaction, SavedProfile};

pub(crate) trait ExactDurabilityError {
    fn durability_unconfirmed(
        revision: u64,
        initial: ProfileStoreError,
        confirmation: ProfileStoreError,
    ) -> Self;

    fn durability_receipt_changed(expected: SavedProfile, actual: Option<SavedProfile>) -> Self;
}

#[derive(Debug)]
pub(crate) struct ExactProfileTransaction<T> {
    value: T,
    profile: Profile,
    durability: ExactDurability,
}

impl<T> ExactProfileTransaction<T> {
    pub(crate) fn into_parts(self) -> (T, Profile, ExactDurability) {
        (self.value, self.profile, self.durability)
    }
}

impl<T> From<ProfileTransaction<T>> for ExactProfileTransaction<T> {
    fn from(transaction: ProfileTransaction<T>) -> Self {
        match transaction {
            ProfileTransaction::Unchanged {
                value,
                profile,
                saved,
            } => Self {
                value,
                profile,
                durability: ExactDurability::Confirmed(saved),
            },
            ProfileTransaction::Committed {
                value,
                profile,
                saved,
            } => Self {
                value,
                profile,
                durability: ExactDurability::Confirmed(Some(saved)),
            },
            ProfileTransaction::CommittedButDurabilityUncertain {
                value,
                profile,
                saved,
                error,
            } => Self {
                value,
                profile,
                durability: ExactDurability::NeedsConfirmation {
                    expected: saved,
                    initial: error,
                },
            },
        }
    }
}

#[derive(Debug)]
pub(crate) enum ExactDurability {
    Confirmed(Option<SavedProfile>),
    NeedsConfirmation {
        expected: SavedProfile,
        initial: ProfileStoreError,
    },
}

impl ExactDurability {
    pub(crate) fn confirm_exact<E>(
        self,
        store: &ProfileStore,
        nickname: &str,
    ) -> Result<Option<SavedProfile>, E>
    where
        E: ExactDurabilityError,
    {
        let (expected, initial) = match self {
            Self::Confirmed(saved) => return Ok(saved),
            Self::NeedsConfirmation { expected, initial } => (expected, initial),
        };

        let actual = store
            .confirm_latest_revision_durable(nickname)
            .map_err(|confirmation| {
                E::durability_unconfirmed(expected.revision, initial, confirmation)
            })?;
        match actual {
            Some(actual) if actual == expected => Ok(Some(actual)),
            actual => Err(E::durability_receipt_changed(expected, actual)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use p5136_profile::{Profile, ProfileMutation};
    use tempfile::tempdir;

    use super::*;

    #[derive(Debug)]
    enum TestError {
        Unconfirmed {
            revision: u64,
            initial: String,
            confirmation: String,
        },
        ReceiptChanged {
            expected: SavedProfile,
            actual: Option<SavedProfile>,
        },
    }

    impl ExactDurabilityError for TestError {
        fn durability_unconfirmed(
            revision: u64,
            initial: ProfileStoreError,
            confirmation: ProfileStoreError,
        ) -> Self {
            Self::Unconfirmed {
                revision,
                initial: initial.to_string(),
                confirmation: confirmation.to_string(),
            }
        }

        fn durability_receipt_changed(
            expected: SavedProfile,
            actual: Option<SavedProfile>,
        ) -> Self {
            Self::ReceiptChanged { expected, actual }
        }
    }

    fn invariant(message: &'static str) -> ProfileStoreError {
        ProfileStoreError::InternalInvariant { message }
    }

    #[test]
    fn confirmed_transaction_preserves_value_profile_and_optional_revision() {
        let profile = Profile::default();
        let saved = SavedProfile {
            nickname: "Rider".to_owned(),
            revision: 7,
            path: PathBuf::from("Launcher.v7.json"),
        };
        let exact = ExactProfileTransaction::from(ProfileTransaction::Committed {
            value: 23_u8,
            profile: profile.clone(),
            saved: saved.clone(),
        });
        let (value, actual_profile, durability) = exact.into_parts();
        let directory = tempdir().unwrap();
        let store = ProfileStore::new(directory.path());

        assert_eq!(value, 23);
        assert_eq!(actual_profile, profile);
        assert_eq!(
            durability
                .confirm_exact::<TestError>(&store, "Rider")
                .unwrap(),
            Some(saved)
        );

        let unchanged = ExactProfileTransaction::from(ProfileTransaction::Unchanged {
            value: (),
            profile,
            saved: None,
        });
        let ((), _, durability) = unchanged.into_parts();
        assert_eq!(
            durability
                .confirm_exact::<TestError>(&store, "Rider")
                .unwrap(),
            None
        );
    }

    #[test]
    fn uncertain_transaction_accepts_only_the_same_immutable_receipt() {
        let directory = tempdir().unwrap();
        let store = ProfileStore::new(directory.path());
        let transaction = store
            .transaction("Rider", |profile| {
                ProfileMutation::changed((), profile.clone())
            })
            .unwrap();
        let ((), _, durability) = ExactProfileTransaction::from(transaction).into_parts();
        let saved = durability
            .confirm_exact::<TestError>(&store, "Rider")
            .unwrap()
            .unwrap();
        let confirmed = ExactDurability::NeedsConfirmation {
            expected: saved.clone(),
            initial: invariant("synthetic initial durability warning"),
        }
        .confirm_exact::<TestError>(&store, "Rider")
        .unwrap()
        .unwrap();
        assert_eq!(confirmed, saved);

        let mismatch = ExactDurability::NeedsConfirmation {
            expected: SavedProfile {
                nickname: confirmed.nickname.clone(),
                revision: confirmed.revision + 1,
                path: confirmed.path.with_file_name("Launcher.v2.json"),
            },
            initial: invariant("synthetic initial durability warning"),
        }
        .confirm_exact::<TestError>(&store, "Rider")
        .unwrap_err();

        match mismatch {
            TestError::ReceiptChanged { expected, actual } => {
                assert_eq!(expected.revision, confirmed.revision + 1);
                assert_eq!(actual, Some(confirmed));
            }
            TestError::Unconfirmed { .. } => panic!("a readable store must report a mismatch"),
        }
    }

    #[test]
    fn confirmation_failure_retains_initial_and_confirmation_context() {
        let directory = tempdir().unwrap();
        let store = ProfileStore::new(directory.path());
        let error = ExactDurability::NeedsConfirmation {
            expected: SavedProfile {
                nickname: "Rider".to_owned(),
                revision: 11,
                path: PathBuf::from("Launcher.v11.json"),
            },
            initial: invariant("synthetic initial durability warning"),
        }
        .confirm_exact::<TestError>(&store, "../invalid")
        .unwrap_err();

        match error {
            TestError::Unconfirmed {
                revision,
                initial,
                confirmation,
            } => {
                assert_eq!(revision, 11);
                assert!(initial.contains("synthetic initial durability warning"));
                assert!(confirmation.contains("nickname"));
            }
            TestError::ReceiptChanged { .. } => {
                panic!("an invalid nickname must fail before receipt comparison");
            }
        }
    }
}
