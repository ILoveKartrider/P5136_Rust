//! Exact, atomic persistence for P5136 favorite-item batches.
//!
//! A stock update is one-way, so there is no wire acknowledgement that can be
//! delayed until storage succeeds. The accepted profile job nevertheless
//! applies the complete batch in one optimistic transaction and confirms the
//! exact immutable revision before session state may be refreshed.
//!
//! An absent Rust marker may still have a C# `Favorite.json` sidecar waiting to
//! be imported. Until that bounded importer exists, both Get (an empty batch)
//! and Update fail closed instead of sealing the marker and losing the import
//! decision.

use p5136_core::item_state_protocol::FavoriteItemChange;
use p5136_profile::{
    FavoriteItemStateError, FavoriteItems, ProfileMutation, ProfileStore, ProfileStoreError,
    SavedProfile, apply_favorite_item_changes,
};
use thiserror::Error;

use crate::profile_durability::{ExactDurabilityError, ExactProfileTransaction};

pub(crate) const FAVORITE_ITEM_UPDATE_OPERATION: &str = "persist favorite-item update";

#[derive(Debug, Error)]
pub enum FavoriteItemPersistError {
    #[error(
        "legacy favorite-item import has not completed; refusing to seal an absent migration marker"
    )]
    MigrationPending,

    #[error(transparent)]
    State(#[from] FavoriteItemStateError),

    #[error("favorite-item profile persistence failed")]
    Store {
        #[source]
        source: Box<ProfileStoreError>,
    },

    #[error("favorite-item transaction completed without an immutable durable revision")]
    MissingDurableRevision,

    #[error("favorite-item transaction completed without canonical favorite-item state")]
    MissingCanonicalState,

    #[error(
        "favorite-item durability confirmation changed immutable receipt from {expected:?} to {actual:?}"
    )]
    DurabilityReceiptChanged {
        expected: Box<SavedProfile>,
        actual: Option<Box<SavedProfile>>,
    },

    #[error(
        "favorite-item profile revision {revision} remained durability-uncertain: initial commit: {initial}; confirmation: {confirmation}"
    )]
    DurabilityUnconfirmed {
        revision: u64,
        initial: Box<ProfileStoreError>,
        #[source]
        confirmation: Box<ProfileStoreError>,
    },
}

impl From<ProfileStoreError> for FavoriteItemPersistError {
    fn from(source: ProfileStoreError) -> Self {
        Self::Store {
            source: Box::new(source),
        }
    }
}

impl ExactDurabilityError for FavoriteItemPersistError {
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

#[derive(Debug)]
pub(crate) struct DurableFavoriteItems {
    saved: SavedProfile,
    items: FavoriteItems,
}

impl DurableFavoriteItems {
    pub(crate) const fn revision(&self) -> u64 {
        self.saved.revision
    }

    pub(crate) const fn items(&self) -> &FavoriteItems {
        &self.items
    }
}

pub(crate) fn persist_favorite_item_changes(
    store: &ProfileStore,
    nickname: &str,
    changes: &[FavoriteItemChange],
    maximum_records: usize,
) -> Result<DurableFavoriteItems, FavoriteItemPersistError> {
    let transaction = store.transaction_with_context(nickname, |current, context| {
        let Some(current_items) = current.favorite_items.as_ref() else {
            return ProfileMutation::Unchanged(Err(FavoriteItemPersistError::MigrationPending));
        };
        match apply_favorite_item_changes(Some(current_items), changes, maximum_records) {
            Ok(next)
                if context.has_immutable_revision()
                    && current.favorite_items.as_ref() == Some(&next) =>
            {
                ProfileMutation::Unchanged(Ok(()))
            }
            Ok(next) => {
                let mut profile = current.clone();
                profile.favorite_items = Some(next);
                ProfileMutation::changed(Ok(()), profile)
            }
            Err(error) => ProfileMutation::Unchanged(Err(error.into())),
        }
    })?;
    let (state, profile, durability) = ExactProfileTransaction::from(transaction).into_parts();
    state?;
    let saved = durability
        .confirm_exact::<FavoriteItemPersistError>(store, nickname)?
        .ok_or(FavoriteItemPersistError::MissingDurableRevision)?;
    let items = profile
        .favorite_items
        .ok_or(FavoriteItemPersistError::MissingCanonicalState)?;
    Ok(DurableFavoriteItems { saved, items })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use p5136_core::item_state_protocol::{
        FavoriteItemChange, FavoriteItemKey, FavoriteItemOperation,
    };
    use p5136_profile::{FavoriteItemStateError, FavoriteItems, ProfileStore};
    use serde_json::json;

    use super::{FavoriteItemPersistError, persist_favorite_item_changes};

    fn add(serial: u16) -> FavoriteItemChange {
        FavoriteItemChange::new(
            FavoriteItemKey::new(3, 1_450, serial),
            FavoriteItemOperation::Add,
        )
    }

    fn canonicalize_empty(store: &ProfileStore, nickname: &str) {
        store.load_or_create(nickname).unwrap();
        store
            .update(nickname, |profile| {
                profile.favorite_items = Some(FavoriteItems::default());
            })
            .unwrap();
    }

    #[test]
    fn complete_batches_commit_atomically_and_retries_reuse_the_revision() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        canonicalize_empty(&store, "FavoriteRider");
        let changes = [add(1), add(2)];

        let first =
            persist_favorite_item_changes(&store, "FavoriteRider", &changes, 1_000).unwrap();
        let first_profile = store.reload("FavoriteRider").unwrap().profile;
        assert_eq!(
            first_profile
                .favorite_items
                .as_ref()
                .expect("successful update canonicalizes the field")
                .as_slice()
                .len(),
            2
        );

        let retry =
            persist_favorite_item_changes(&store, "favoriterider", &changes, 1_000).unwrap();
        let retry_profile = store.reload("FavoriteRider").unwrap().profile;
        assert_eq!(retry_profile.favorite_items, first_profile.favorite_items);
        assert_eq!(retry.revision(), first.revision());
    }

    #[test]
    fn absent_migration_marker_fails_closed_without_publishing_a_revision() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let initial = store.load_or_create("EmptyFavoriteRider").unwrap();
        assert!(initial.profile.favorite_items.is_none());

        assert!(matches!(
            persist_favorite_item_changes(&store, "EmptyFavoriteRider", &[], 1_000),
            Err(FavoriteItemPersistError::MigrationPending)
        ));
        let after = store.reload("EmptyFavoriteRider").unwrap();
        assert_eq!(after.revision, initial.revision);
        assert_eq!(after.profile, initial.profile);
    }

    #[test]
    fn idempotent_update_canonicalizes_a_matching_legacy_snapshot_once() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("LegacyFavoriteRider");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("Launcher.json"),
            serde_json::to_vec(&json!({
                "P5136RustFavoriteItems": [
                    {"ItemCatID": 3, "ItemID": 1450, "ItemSN": 1}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let store = ProfileStore::new(root.path());
        let legacy = store.load_or_create("LegacyFavoriteRider").unwrap();
        assert_eq!(legacy.revision, None);

        let first =
            persist_favorite_item_changes(&store, "LegacyFavoriteRider", &[add(1)], 1_000).unwrap();
        assert_eq!(first.revision(), 1);
        assert_eq!(
            store
                .reload("LegacyFavoriteRider")
                .unwrap()
                .profile
                .favorite_items
                .as_ref()
                .unwrap()
                .as_slice(),
            &[FavoriteItemKey::new(3, 1_450, 1)]
        );

        let retry =
            persist_favorite_item_changes(&store, "LegacyFavoriteRider", &[add(1)], 1_000).unwrap();
        assert_eq!(retry.revision(), first.revision());
    }

    #[test]
    fn several_stock_sized_batches_can_grow_the_collection_beyond_two_hundred() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        canonicalize_empty(&store, "LargeFavoriteRider");
        let first = (0..200).map(add).collect::<Vec<_>>();
        let second = (200..400).map(add).collect::<Vec<_>>();

        persist_favorite_item_changes(&store, "LargeFavoriteRider", &first, 1_000).unwrap();
        let durable =
            persist_favorite_item_changes(&store, "LargeFavoriteRider", &second, 1_000).unwrap();
        let loaded = store.reload("LargeFavoriteRider").unwrap();
        assert_eq!(
            loaded
                .profile
                .favorite_items
                .expect("successful update canonicalizes the field")
                .as_slice()
                .len(),
            400
        );
        assert_eq!(loaded.revision, Some(durable.revision()));
    }

    #[test]
    fn final_cap_rejection_leaves_the_exact_profile_revision_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        canonicalize_empty(&store, "BoundedFavoriteRider");
        let initial = store.reload("BoundedFavoriteRider").unwrap();
        persist_favorite_item_changes(&store, "BoundedFavoriteRider", &[add(1), add(2)], 2)
            .unwrap();
        let before = store.reload("BoundedFavoriteRider").unwrap();
        assert_ne!(before.revision, initial.revision);

        assert!(matches!(
            persist_favorite_item_changes(&store, "BoundedFavoriteRider", &[add(3)], 2),
            Err(FavoriteItemPersistError::State(
                FavoriteItemStateError::TooManyItems {
                    count: 3,
                    maximum: 2
                }
            ))
        ));
        let after = store.reload("BoundedFavoriteRider").unwrap();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.profile, before.profile);
    }
}
