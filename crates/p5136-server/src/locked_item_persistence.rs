//! Exact, atomic persistence for P5136 locked-item batches.
//!
//! Locked and favorite items share the same bounded ordered-set value
//! abstraction, but remain distinct canonical profile fields. A complete
//! update batch is applied in one profile transaction and the exact immutable
//! revision is confirmed before session state can observe it. An unresolved
//! C# `Locked.json` is captured through the run lease without following links;
//! import, canonical marker, and the incoming batch share one revision.

use p5136_core::item_state_protocol::FavoriteItemChange;
use p5136_profile::{
    DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS, FavoriteItemStateError, LockedItemImportError,
    LockedItems, ProfileStore, ProfileStoreError, RaceRunLease, SavedProfile,
    apply_item_collection_changes,
};
use thiserror::Error;

use crate::profile_durability::{ExactDurabilityError, ExactProfileTransaction};

pub(crate) const LOCKED_ITEM_UPDATE_OPERATION: &str = "persist locked-item update";

#[derive(Debug, Error)]
pub enum LockedItemPersistError {
    #[error(transparent)]
    Import(#[from] LockedItemImportError),

    #[error(transparent)]
    State(#[from] FavoriteItemStateError),

    #[error("locked-item profile persistence failed")]
    Store {
        #[source]
        source: Box<ProfileStoreError>,
    },

    #[error("locked-item transaction completed without an immutable durable revision")]
    MissingDurableRevision,

    #[error("locked-item transaction completed without canonical locked-item state")]
    MissingCanonicalState,

    #[error(
        "locked-item durability confirmation changed immutable receipt from {expected:?} to {actual:?}"
    )]
    DurabilityReceiptChanged {
        expected: Box<SavedProfile>,
        actual: Option<Box<SavedProfile>>,
    },

    #[error(
        "locked-item profile revision {revision} remained durability-uncertain: initial commit: {initial}; confirmation: {confirmation}"
    )]
    DurabilityUnconfirmed {
        revision: u64,
        initial: Box<ProfileStoreError>,
        #[source]
        confirmation: Box<ProfileStoreError>,
    },
}

impl From<ProfileStoreError> for LockedItemPersistError {
    fn from(source: ProfileStoreError) -> Self {
        Self::Store {
            source: Box::new(source),
        }
    }
}

impl ExactDurabilityError for LockedItemPersistError {
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
pub(crate) struct DurableLockedItems {
    saved: SavedProfile,
    items: LockedItems,
}

impl DurableLockedItems {
    pub(crate) const fn revision(&self) -> u64 {
        self.saved.revision
    }

    pub(crate) const fn items(&self) -> &LockedItems {
        &self.items
    }
}

pub(crate) fn persist_locked_item_changes(
    store: &ProfileStore,
    lease: &RaceRunLease,
    nickname: &str,
    changes: &[FavoriteItemChange],
    maximum_records: usize,
) -> Result<DurableLockedItems, LockedItemPersistError> {
    let transaction =
        store.transaction_with_legacy_locked_items(lease, nickname, |current_items, origin| {
            let next =
                apply_item_collection_changes(Some(current_items), changes, maximum_records)?;
            let maximum = maximum_records.min(DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS);
            if origin.is_legacy_sidecar() && next.len() > maximum {
                return Err(FavoriteItemStateError::TooManyItems {
                    count: next.len(),
                    maximum,
                });
            }
            Ok(((), next))
        })?;
    let (state, profile, durability) = ExactProfileTransaction::from(transaction).into_parts();
    state?;
    let saved = durability
        .confirm_exact::<LockedItemPersistError>(store, nickname)?
        .ok_or(LockedItemPersistError::MissingDurableRevision)?;
    let items = profile
        .locked_items
        .ok_or(LockedItemPersistError::MissingCanonicalState)?;
    Ok(DurableLockedItems { saved, items })
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};

    use p5136_core::item_state_protocol::{
        FavoriteItemChange, FavoriteItemKey, FavoriteItemOperation,
    };
    use p5136_profile::{LockedItemImportError, ProfileStore, item_collection_snapshot};

    use super::{LockedItemPersistError, persist_locked_item_changes};

    #[test]
    fn locked_batches_are_atomic_durable_and_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        let key = FavoriteItemKey::new(3, 1_450, 2);

        let first = persist_locked_item_changes(
            &store,
            &lease,
            "LockedRider",
            &[FavoriteItemChange::new(key, FavoriteItemOperation::Add)],
            100,
        )
        .unwrap();
        assert_eq!(first.items().as_slice(), [key]);

        let retry = persist_locked_item_changes(
            &store,
            &lease,
            "LockedRider",
            &[FavoriteItemChange::new(key, FavoriteItemOperation::Add)],
            100,
        )
        .unwrap();
        assert_eq!(retry.items().as_slice(), [key]);

        let removed = persist_locked_item_changes(
            &store,
            &lease,
            "LockedRider",
            &[FavoriteItemChange::new(key, FavoriteItemOperation::Remove)],
            100,
        )
        .unwrap();
        assert!(removed.items().is_empty());
        let loaded = store.reload("LockedRider").unwrap();
        assert!(item_collection_snapshot(loaded.profile.locked_items.as_ref()).is_empty());
    }

    #[test]
    fn locked_sidecar_import_and_first_update_share_one_revision() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("LockedImportRider");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("Launcher.json"), b"{}").unwrap();
        fs::write(
            directory.join("Locked.json"),
            br#"[{"ItemCatID":3,"ItemID":1450,"ItemSN":1}]"#,
        )
        .unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        let added = FavoriteItemKey::new(3, 1_450, 2);

        let receipt = persist_locked_item_changes(
            &store,
            &lease,
            "LockedImportRider",
            &[FavoriteItemChange::new(added, FavoriteItemOperation::Add)],
            100,
        )
        .unwrap();

        assert_eq!(receipt.revision(), 1);
        assert_eq!(
            receipt.items().as_slice(),
            [
                FavoriteItemKey::new(3, 1_450, 1),
                FavoriteItemKey::new(3, 1_450, 2),
            ]
        );
        let loaded = store.reload("LockedImportRider").unwrap();
        assert_eq!(loaded.profile.locked_items, Some(receipt.items().clone()));
    }

    #[test]
    fn nonregular_locked_sidecar_fails_closed_without_canonicalizing_state() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("NonregularLockedRider");
        fs::create_dir_all(directory.join("Locked.json")).unwrap();
        fs::write(directory.join("Launcher.json"), b"{}").unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();

        assert!(matches!(
            persist_locked_item_changes(&store, &lease, "NonregularLockedRider", &[], 100,),
            Err(LockedItemPersistError::Import(
                LockedItemImportError::InvalidStorageEntry { .. }
            ))
        ));
        assert!(
            store
                .reload("NonregularLockedRider")
                .unwrap()
                .profile
                .locked_items
                .is_none()
        );
    }

    #[test]
    fn malformed_and_oversized_locked_sidecars_fail_without_canonicalizing_state() {
        let malformed_root = tempfile::tempdir().unwrap();
        let malformed_directory = malformed_root.path().join("MalformedLockedRider");
        fs::create_dir_all(&malformed_directory).unwrap();
        fs::write(malformed_directory.join("Launcher.json"), b"{}").unwrap();
        fs::write(malformed_directory.join("Locked.json"), b"{not-json").unwrap();
        let malformed_store = ProfileStore::new(malformed_root.path());
        let malformed_lease = malformed_store.acquire_race_run_lease().unwrap();
        assert!(matches!(
            persist_locked_item_changes(
                &malformed_store,
                &malformed_lease,
                "MalformedLockedRider",
                &[],
                100,
            ),
            Err(LockedItemPersistError::Import(
                LockedItemImportError::Json { .. }
            ))
        ));
        assert!(
            malformed_store
                .reload("MalformedLockedRider")
                .unwrap()
                .profile
                .locked_items
                .is_none()
        );

        let oversized_root = tempfile::tempdir().unwrap();
        let oversized_directory = oversized_root.path().join("OversizedLockedRider");
        fs::create_dir_all(&oversized_directory).unwrap();
        fs::write(oversized_directory.join("Launcher.json"), b"{}").unwrap();
        File::create(oversized_directory.join("Locked.json"))
            .unwrap()
            .set_len(65)
            .unwrap();
        let oversized_store = ProfileStore::with_maximum_bytes(oversized_root.path(), 64);
        let oversized_lease = oversized_store.acquire_race_run_lease().unwrap();
        assert!(matches!(
            persist_locked_item_changes(
                &oversized_store,
                &oversized_lease,
                "OversizedLockedRider",
                &[],
                100,
            ),
            Err(LockedItemPersistError::Import(
                LockedItemImportError::TooLarge { .. }
            ))
        ));
        assert!(
            oversized_store
                .reload("OversizedLockedRider")
                .unwrap()
                .profile
                .locked_items
                .is_none()
        );
    }
}
